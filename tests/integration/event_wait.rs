//! Commit-event and handshake waits: inotify arm, recheck,
//! validate-after-wake, fail-loud bound (no clock-tuned waits).
//!
//! Split from `helpers.rs` at the file-size limit; re-exported
//! there so existing `helpers::*` call sites keep working.

use std::time::{Duration, Instant};

pub fn wait_attestation(id: &str, container: &str) -> cistella::attestation::Attestation {
    let base = cistella::lock::runtime_base();
    let state_dir = cistella::lock::PrepareLease::state_dir(&base);
    // Open through the anchored discipline (creates 0700
    // seat-owned on first use) so the watched directory is the
    // same trusted object the owner publishes into.
    let _anchored =
        cistella::guest_anchor::open_state_dir(&state_dir).expect("attestation state dir opens");
    await_observed(
        &state_dir,
        &format!("launch attestation for session {id}"),
        Duration::from_secs(60),
        || read_valid_attestation(&base, id, container),
    )
}

/// Waits for a readiness file with exact expected content:
/// same arm → recheck → validate-after-wake shape as
/// `wait_attestation`, for peer-published handshake signals
/// (e.g. await admission). Returns the observed content.
pub fn wait_readiness_file(
    dir: &std::path::Path,
    name: &str,
    expected: &str,
    timeout: Duration,
) -> String {
    let content = await_observed(dir, &format!("readiness file {name}"), timeout, || {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .filter(|text| !text.is_empty())
    });
    assert_eq!(
        content, expected,
        "readiness file {name} names the admitted op, got: {content}"
    );
    content
}

/// Inotify arm → recheck → validate-after-wake core shared by
/// commit-event and handshake waits: `observe` decides
/// validity (content/identity, never mere CREATE); the bound
/// is fail-loud only. Raw libc inotify (no new dependency).
fn await_observed<T>(
    dir: &std::path::Path,
    what: &str,
    timeout: Duration,
    observe: impl Fn() -> Option<T>,
) -> T {
    use std::os::unix::io::RawFd;
    // Recheck before subscribing: the event may already exist.
    if let Some(valid) = observe() {
        return valid;
    }
    // SAFETY: straight-line libc inotify init with checked
    // return; CLOEXEC|NONBLOCK, closed on every exit path below.
    let fd: RawFd = unsafe { ::libc::inotify_init1(::libc::IN_CLOEXEC | ::libc::IN_NONBLOCK) };
    if fd < 0 {
        panic!("inotify init failed");
    }
    struct Closer(RawFd);
    impl Drop for Closer {
        fn drop(&mut self) {
            unsafe {
                ::libc::close(self.0);
            }
        }
    }
    let _closer = Closer(fd);
    // SAFETY: watch the dir for publication events; the path is
    // NUL-free by construction, checked here rather than assumed.
    use std::os::unix::ffi::OsStrExt;
    let bytes = dir.as_os_str().as_bytes();
    if bytes.contains(&0) {
        panic!("inotify path contains NUL");
    }
    let mut path_buf = bytes.to_vec();
    path_buf.push(0);
    // SAFETY: NUL-terminated path, checked fd, checked return.
    let watch = unsafe {
        ::libc::inotify_add_watch(
            fd,
            path_buf.as_ptr() as *const ::libc::c_char,
            ::libc::IN_CLOSE_WRITE | ::libc::IN_CREATE | ::libc::IN_MOVED_TO,
        )
    };
    if watch < 0 {
        panic!("inotify watch failed on {}", dir.display());
    }
    // Recheck after subscribing: closes the arm-then-miss gap.
    if let Some(valid) = observe() {
        return valid;
    }
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            panic!("{what} never observed (fail-loud bound)");
        }
        // SAFETY: single-fd poll with bounded timeout; revents
        // ignored (any readiness or timeout re-drives the
        // observe below, which is the actual condition).
        let mut poll_fd = ::libc::pollfd {
            fd,
            events: ::libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as ::libc::c_int;
        let ready = unsafe { ::libc::poll(&mut poll_fd, 1, timeout_ms) };
        if ready < 0 {
            let errno = unsafe { *::libc::__errno_location() };
            if errno == ::libc::EINTR {
                continue;
            }
            panic!("inotify poll failed: errno {errno}");
        }
        if ready > 0 {
            // Drain available events (content ignored — the
            // observe validates identity, never mere CREATE).
            // SAFETY: bounded buffer, checked fd, short read is
            // a complete drain for NONBLOCK semantics.
            loop {
                let n =
                    unsafe { ::libc::read(fd, buf.as_mut_ptr() as *mut ::libc::c_void, buf.len()) };
                if n <= 0 {
                    break;
                }
            }
        }
        if let Some(valid) = observe() {
            return valid;
        }
    }
}

/// Reads and validates one attestation: parseable, id-matching,
/// container-matching, incarnation-bound (native commits).
/// Returns None when absent or not-yet-valid — the caller, not
/// this probe, decides failure.
fn read_valid_attestation(
    base: &std::path::Path,
    id: &str,
    container: &str,
) -> Option<cistella::attestation::Attestation> {
    let attestation = cistella::attestation::read(base, id).ok()??;
    if attestation.container_name != container {
        return None;
    }
    attestation.incarnation.as_ref()?;
    Some(attestation)
}
