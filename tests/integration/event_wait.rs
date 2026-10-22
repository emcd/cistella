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
    // Recheck before subscribing: the event may already exist.
    if let Some(valid) = observe() {
        return valid;
    }
    let armed = arm_watch(dir);
    await_armed(&armed, what, timeout, observe)
}

/// An armed inotify watch: the subscription exists, so a later
/// `await_armed` exercises the post-subscription recheck path
/// (never the pre-arm one). Closed on drop.
pub struct ArmedWatch {
    fd: std::os::unix::io::RawFd,
}

impl Drop for ArmedWatch {
    fn drop(&mut self) {
        unsafe {
            ::libc::close(self.fd);
        }
    }
}

/// Arms an inotify watch on `dir` (CLOSE_WRITE|CREATE|MOVED_TO)
/// without waiting: split from `await_observed` so pins can
/// choreograph the arm-then-event ordering deterministically
/// (stage after arm, never sleep-tuned).
pub fn arm_watch(dir: &std::path::Path) -> ArmedWatch {
    use std::os::unix::io::RawFd;
    // SAFETY: straight-line libc inotify init with checked
    // return; CLOEXEC|NONBLOCK, owned by the guard below.
    let fd: RawFd = unsafe { ::libc::inotify_init1(::libc::IN_CLOEXEC | ::libc::IN_NONBLOCK) };
    if fd < 0 {
        panic!("inotify init failed");
    }
    // SAFETY: watch the dir for publication events; the path is
    // NUL-free by construction, checked here rather than assumed.
    use std::os::unix::ffi::OsStrExt;
    let bytes = dir.as_os_str().as_bytes();
    if bytes.contains(&0) {
        unsafe {
            ::libc::close(fd);
        }
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
        unsafe {
            ::libc::close(fd);
        }
        panic!("inotify watch failed on {}", dir.display());
    }
    ArmedWatch { fd }
}

/// Waits on an already-armed watch: post-subscription recheck
/// first (closes the arm-then-miss gap), then the poll loop.
/// Every wake re-drives `observe`; the bound fires fail-loud.
pub fn await_armed<T>(
    armed: &ArmedWatch,
    what: &str,
    timeout: Duration,
    observe: impl Fn() -> Option<T>,
) -> T {
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
            fd: armed.fd,
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
                let n = unsafe {
                    ::libc::read(armed.fd, buf.as_mut_ptr() as *mut ::libc::c_void, buf.len())
                };
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
/// container-matching, incarnation-bound (native commits), AND
/// live-equal to the current backend incarnation. Every
/// non-valid outcome panics immediately: corrupt, mismatched,
/// stale, unincarnated, and unqueryable are all hopeless —
/// atomic publication means a present file is always complete,
/// a write-once marker never becomes current, and no attestation
/// event will ever make an unobservable backend observable —
/// so waiting out the bound would be silent wrongness, not
/// patience. A safe false-negative (loud failure) beats a
/// hopeful wait. The product would fail typed on the same
/// facts; the harness fails loud on them.
fn read_valid_attestation(
    base: &std::path::Path,
    id: &str,
    container: &str,
) -> Option<cistella::attestation::Attestation> {
    read_valid_attestation_with(base, id, container, || {
        cistella::attestation::query_incarnation(container).map_err(|error| error.to_string())
    })
}

/// Validation with an injected identity source: the pure
/// currency classification (`decide_currency`) behind a
/// caller-supplied live query. Production passes the real
/// backend query; pins inject fabricated outcomes, so all four
/// deterministic observation controls run with no backend.
fn read_valid_attestation_with(
    base: &std::path::Path,
    id: &str,
    container: &str,
    query: impl FnOnce() -> Result<cistella::attestation::Incarnation, String>,
) -> Option<cistella::attestation::Attestation> {
    use cistella::attestation::{Currency, decide_currency};
    let attestation = match cistella::attestation::read(base, id) {
        Ok(found) => found?,
        Err(error) => panic!("attestation for session {id} unreadable: {error}"),
    };
    // Container binding before any backend contact: a mismatch
    // fails without running the live query at all.
    if attestation.container_name != container {
        panic!(
            "attestation for session {id} names a different container (possible out-of-band recreate)"
        );
    }
    let bound = attestation.incarnation.as_ref().unwrap_or_else(|| {
        panic!("attestation for session {id} carries no incarnation (foreign commitment)")
    });
    let live = match query() {
        Ok(observed) => Some(observed),
        Err(error) => panic!("incarnation unqueryable for session {id}: {error}"),
    };
    match decide_currency(true, bound, live.as_ref()) {
        Currency::Current => Some(attestation),
        Currency::Stale => {
            panic!("attestation for session {id} is stale (container recreated or restarted)")
        }
        Currency::Unknown => {
            panic!("incarnation unqueryable for session {id} (unknown)")
        }
        Currency::Mismatched => {
            unreachable!("container binding checked above")
        }
    }
}

#[test]
fn await_observed_pins_arm_recheck_validate() {
    // Helper-level pin for the event-wait acceptance shape (no
    // podman, no systemd): unrelated CREATEs never satisfy the
    // wait, a pre-arm commit is caught by the recheck, and the
    // fail-loud bound does not fire on a genuine commit. A
    // thread stages the events; the assertions pin identity
    // (content), never timing.
    let dir = tempfile::tempdir().expect("tmpdir");
    // Pre-arm commit: staged before the arm, caught by the
    // first recheck (no event needed). Short bound so a
    // missing recheck fails instead of hanging the suite.
    std::fs::write(dir.path().join("early"), b"early-content").expect("stage");
    let got: String = await_observed(
        dir.path(),
        "pre-armed commit",
        Duration::from_secs(2),
        || {
            std::fs::read_to_string(dir.path().join("early"))
                .ok()
                .filter(|text| !text.is_empty())
        },
    );
    assert_eq!(got, "early-content", "pre-arm commit caught by recheck");
    // Unrelated CREATE first, genuine commit second: the wait
    // must ignore the former and return the latter's content.
    // (Proves wake-on-unrelated does not satisfy, and the bound
    // does not fire on a genuine commit — return itself is the
    // proof, no timing asserts.) Staging is causally ordered
    // after the arm (same-thread spawn order), never
    // sleep-tuned: the target write cannot precede the
    // subscription.
    let armed = arm_watch(dir.path());
    std::fs::write(dir.path().join("unrelated"), b"noise").expect("stage noise");
    let staged = dir.path().to_path_buf();
    std::thread::spawn(move || {
        std::fs::write(staged.join("target"), b"target-content").expect("stage target");
    });
    let got: String = await_armed(&armed, "genuine commit", Duration::from_secs(10), || {
        std::fs::read_to_string(dir.path().join("target"))
            .ok()
            .filter(|text| !text.is_empty())
    });
    assert_eq!(got, "target-content", "only the validated commit satisfies");
}

#[test]
#[should_panic(expected = "never observed (fail-loud bound)")]
fn await_unrelated_wake_never_satisfies() {
    // Negative control: the unrelated CREATE lands AFTER the
    // subscription, so its wake genuinely fires — yet the
    // predicate only accepts the absent target, so the bound
    // fires fail-loud instead of returning or hanging.
    // Deterministic: no target file exists at all.
    let dir = tempfile::tempdir().expect("tmpdir");
    let armed = arm_watch(dir.path());
    std::fs::write(dir.path().join("unrelated"), b"noise").expect("stage noise");
    let _: () = await_armed(&armed, "unrelated-only", Duration::from_millis(200), || {
        // The unrelated file exists and its wake fires, but the
        // predicate only accepts the absent target: never
        // satisfied, so the bound fires.
        let _ = std::fs::read_to_string(dir.path().join("unrelated")).ok();
        None::<()>
    });
}

#[test]
#[should_panic(expected = "never observed (fail-loud bound)")]
fn await_observed_bound_fails_loud_without_commit() {
    // Negative control for the backstop: no commit ever lands,
    // so the bound fires with the fail-loud message instead of
    // returning or hanging. Short bound keeps it fast.
    let dir = tempfile::tempdir().expect("tmpdir");
    let _: Option<()> = await_observed(
        dir.path(),
        "absent commit",
        Duration::from_millis(200),
        || None,
    );
}

#[test]
fn await_armed_recheck_precedes_poll() {
    // Deterministic pin for the post-subscription recheck: the
    // target is staged AFTER the arm (same thread, so the
    // subscription strictly precedes it) and the budget is zero
    // — await_armed must return via the recheck without ever
    // polling. Deleting the recheck turns this into an
    // immediate bound failure. No sleeps, no timing.
    let dir = tempfile::tempdir().expect("tmpdir");
    let armed = arm_watch(dir.path());
    std::fs::write(dir.path().join("target"), b"recheck-content").expect("stage target");
    let got: String = await_armed(&armed, "recheck-only", Duration::ZERO, || {
        std::fs::read_to_string(dir.path().join("target"))
            .ok()
            .filter(|text| !text.is_empty())
    });
    assert_eq!(
        got, "recheck-content",
        "post-subscription recheck satisfies"
    );
}

fn fixture_commitment(
    base: &std::path::Path,
    id: &str,
    container: &str,
) -> cistella::attestation::Incarnation {
    let incarnation = cistella::attestation::Incarnation {
        container_id: "fixture-container-id".to_string(),
        started_at: "fixture-started-at".to_string(),
    };
    cistella::attestation::publish(
        base,
        &cistella::attestation::Attestation {
            session_id: id.to_string(),
            container_name: container.to_string(),
            attempt_key: "fixture-key".to_string(),
            argv: vec!["sleep".to_string()],
            execution_handle: "fixture-exec".to_string(),
            incarnation: Some(incarnation.clone()),
        },
    )
    .expect("fixture commitment publishes");
    incarnation
}

#[test]
fn read_valid_attestation_accepts_current_identity() {
    // Injected live query returning the bound identity: the
    // helper returns the attestation (no podman involved).
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();
    let id = "fixture-current-1";
    let container = format!("cistella-{id}");
    let bound = fixture_commitment(base, id, &container);
    let found =
        read_valid_attestation_with(base, id, &container, || Ok::<_, String>(bound.clone()))
            .expect("current identity validates");
    assert_eq!(found.session_id, id);
}

#[test]
#[should_panic(expected = "is stale")]
fn read_valid_attestation_rejects_stale_identity() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();
    let id = "fixture-stale-1";
    let container = format!("cistella-{id}");
    fixture_commitment(base, id, &container);
    let stale = cistella::attestation::Incarnation {
        container_id: "replacement-id".to_string(),
        started_at: "fixture-started-at".to_string(),
    };
    let _ = read_valid_attestation_with(base, id, &container, || Ok::<_, String>(stale));
}

#[test]
#[should_panic(expected = "unqueryable")]
fn read_valid_attestation_rejects_unknown_identity() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();
    let id = "fixture-unknown-1";
    let container = format!("cistella-{id}");
    fixture_commitment(base, id, &container);
    let _ = read_valid_attestation_with(base, id, &container, || {
        Err::<cistella::attestation::Incarnation, String>("backend hiccup".to_string())
    });
}

#[test]
#[should_panic(expected = "different container")]
fn read_valid_attestation_rejects_container_mismatch() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();
    let id = "fixture-mismatch-1";
    let container = format!("cistella-{id}");
    let bound = fixture_commitment(base, id, &container);
    // The query must never run on a mismatch: the closure
    // panics if called, proving the ordering.
    let _ = read_valid_attestation_with(base, id, "cistella-some-other-session", || {
        panic!("live query must not run after a container mismatch");
        #[allow(unreachable_code)]
        Ok::<_, String>(bound.clone())
    });
}

#[test]
#[should_panic(expected = "foreign commitment")]
fn read_valid_attestation_rejects_unincarnated_commitment() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();
    let id = "fixture-foreign-1";
    let container = format!("cistella-{id}");
    cistella::attestation::publish(
        base,
        &cistella::attestation::Attestation {
            session_id: id.to_string(),
            container_name: container.clone(),
            attempt_key: "fixture-key".to_string(),
            argv: vec!["sleep".to_string()],
            execution_handle: "fixture-exec".to_string(),
            incarnation: None,
        },
    )
    .expect("foreign fixture publishes");
    let _ = read_valid_attestation_with(base, id, &container, || {
        Ok::<_, String>(cistella::attestation::Incarnation {
            container_id: "whatever".to_string(),
            started_at: "whenever".to_string(),
        })
    });
}
