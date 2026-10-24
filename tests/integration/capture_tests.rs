//! Deterministic controls for the shared bounded capture:
//! every interleaving the collector promises is pinned here
//! with no backend, no timing, and no sleeps as ordering
//! (bounded convergence polls only). Split from `capture.rs`
//! at the file-size limit: the collector stays
//! dependency-light while the controls grow.

use std::os::unix::io::IntoRawFd;
use std::time::{Duration, Instant};

use super::capture::*;
/// Reaps a forked test child (blocking): the deterministic
/// controls below reap before draining, proving the
/// exit-before-any-read interleaving converges with complete
/// bytes. Panics with context on failure (controls, not the
/// harness paths — those stay best-effort and report).
fn reap_child(child: nix::unistd::Pid) {
    match nix::sys::wait::waitpid(child, None) {
        Ok(_) => {}
        Err(error) => panic!("control child failed to reap: {error}"),
    }
}
/// Bounded spin until child_gone converges (controls only):
/// the child code paths below are exit-only, so convergence
/// is guaranteed; the bound fails loud instead of hanging a
/// wedged child.
fn spin_until_gone(
    child: nix::unistd::Pid,
    reaped: &mut bool,
    exited_at: &mut Option<Instant>,
    disposition: &mut ChildDisposition,
) {
    for _ in 0..1000 {
        if child_gone(child, reaped, exited_at, disposition) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("control child never converged");
}
#[test]
fn child_gone_records_exit_code_at_first_consumption() {
    // Forked child exits 7 (exit-only path, so convergence is
    // guaranteed): the first terminal observation records the
    // code, and a later ECHILD (status already consumed)
    // keeps it — first consumption wins, never overwritten
    // with Unavailable.
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            let mut reaped = false;
            let mut exited_at = None;
            let mut disposition = ChildDisposition::Unknown;
            spin_until_gone(child, &mut reaped, &mut exited_at, &mut disposition);
            assert!(reaped, "terminal observation reaps");
            assert_eq!(disposition, ChildDisposition::Exited(7));
            assert!(exited_at.is_some(), "exit observation timestamped");
            // Reap bookkeeping already consumed: ECHILD now,
            // and the recorded code must survive it.
            assert!(child_gone(
                child,
                &mut reaped,
                &mut exited_at,
                &mut disposition
            ));
            assert_eq!(disposition, ChildDisposition::Exited(7));
        }
        nix::unistd::ForkResult::Child => unsafe { libc::_exit(7) },
    }
}

#[test]
fn child_gone_records_signal_death() {
    // Forked child raises SIGTERM to itself: disposition is
    // the signal, distinct from an exit code.
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            let mut reaped = false;
            let mut exited_at = None;
            let mut disposition = ChildDisposition::Unknown;
            spin_until_gone(child, &mut reaped, &mut exited_at, &mut disposition);
            assert!(reaped, "terminal observation reaps");
            assert_eq!(
                disposition,
                ChildDisposition::Signaled(libc::SIGTERM),
                "signal death recorded, not an exit code"
            );
        }
        nix::unistd::ForkResult::Child => unsafe {
            libc::raise(libc::SIGTERM);
            libc::_exit(3);
        },
    }
}

#[test]
fn child_gone_reports_live_child_without_consuming() {
    // Forked child sleeps well past the observation: a single
    // poll reports not-gone, Unknown preserved, nothing
    // reaped. Cleanup kills and reaps (bounded, then verified
    // gone) so no sleeper interferes with sibling tests.
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            let mut reaped = false;
            let mut exited_at = None;
            let mut disposition = ChildDisposition::Unknown;
            assert!(
                !child_gone(child, &mut reaped, &mut exited_at, &mut disposition),
                "live child is not gone"
            );
            assert!(!reaped, "nothing reaped");
            assert_eq!(disposition, ChildDisposition::Unknown);
            assert!(exited_at.is_none(), "no exit observed");
            let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
            reap_child(child);
        }
        nix::unistd::ForkResult::Child => unsafe {
            // Shed inherited sibling fds: this sleeper outlives
            // every capture window and must not hold another
            // test's EOF open.
            close_unrelated_fds(&[]);
            libc::sleep(30);
            libc::_exit(0);
        },
    }
}

#[test]
fn capture_descendant_held_pipe_grace_expires_bounded() {
    // A writer held open past the direct child's reaped exit:
    // the collector must report GraceExpired inside the budget
    // — never hang on the held descriptor. The holder is the
    // test itself (a dup kept open across the reap); from the
    // collector's side this is indistinguishable from a
    // descendant-held descriptor (reaped exit plus an open
    // writer), and it creates no orphans that could hold
    // sibling tests' descriptors open across parallel tests.
    let (read, write) = nix::unistd::pipe().expect("pipe");
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            reap_child(child);
            // Hold a duplicate write end open past the reap:
            // the session's writer is gone, a writer remains.
            use std::os::unix::io::AsRawFd;
            let held = unsafe { libc::dup(write.as_raw_fd()) };
            assert!(held >= 0, "dup holds the write end");
            drop(write);
            let file = std::fs::File::from(read);
            set_nonblocking(&file);
            let start = Instant::now();
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(1),
                CAPTURE_CAP,
                |_| false,
            );
            assert_eq!(outcome.end, CaptureEnd::GraceExpired);
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "grace expiry stays inside the budget"
            );
            unsafe { libc::close(held) };
        }
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::close(read.into_raw_fd());
            let _ = nix::unistd::close(write.into_raw_fd());
            unsafe { libc::_exit(0) };
        }
    }
}

#[test]
fn capture_pty_eio_after_reaped_slave_close() {
    // PTY-specific: the child writes, exits, and its slave
    // closes; the parent reaps first (deterministic), then
    // drains. End of stream arrives as EOF or as EIO with the
    // child already reaped — either way the queued bytes must
    // be complete. An EIO acceptance requires child_reaped.
    match unsafe { nix::pty::forkpty(None, None).expect("forkpty") } {
        nix::pty::ForkptyResult::Parent { child, master } => {
            reap_child(child);
            let file = std::fs::File::from(master);
            set_nonblocking(&file);
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(2),
                CAPTURE_CAP,
                |bytes| String::from_utf8_lossy(bytes).contains("PTY-DONE"),
            );
            assert!(
                outcome.bytes == b"PTY-DONE\n" || outcome.bytes == b"PTY-DONE\r\n",
                "queued bytes complete (pty may translate LF to CRLF), got: {:?}",
                outcome.bytes
            );
            assert_eq!(
                outcome.disposition,
                ChildDisposition::Unavailable,
                "pre-reaped child reads as reaped-elsewhere"
            );
            assert!(
                matches!(
                    outcome.end,
                    CaptureEnd::Eof | CaptureEnd::PtyEio { child_reaped: true }
                ),
                "end of stream is EOF or reaped-EIO, got: {:?}",
                outcome.end
            );
            assert!(outcome.matched, "settle predicate fires");
        }
        nix::pty::ForkptyResult::Child => {
            // Minimal post-fork body: async-signal-safe only.
            let message = b"PTY-DONE\n";
            unsafe {
                libc::write(
                    libc::STDOUT_FILENO,
                    message.as_ptr() as *const std::os::raw::c_void,
                    message.len() as libc::size_t,
                );
                libc::_exit(0);
            }
        }
    }
}

#[test]
fn capture_pty_descendant_holds_slave_grace_expires() {
    // A slave held open past the direct child's reaped exit:
    // GraceExpired inside the budget, never a hang. The holder
    // is the test itself (openpty pair; the slave stays open
    // across the child reap), which is indistinguishable from
    // a descendant-held slave at the collector boundary — and
    // creates no orphans that could hold sibling tests'
    // descriptors open across parallel tests. (A forkpty
    // grandchild would also be killed by SIGHUP on session
    // leader exit; the holder here sidesteps that.)
    let opened = nix::pty::openpty(None, None).expect("openpty");
    let master = opened.master;
    let slave = opened.slave;
    let child = match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => child,
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::close(master.into_raw_fd());
            let _ = nix::unistd::close(slave.into_raw_fd());
            unsafe { libc::_exit(0) };
        }
    };
    reap_child(child);
    let file = std::fs::File::from(master);
    set_nonblocking(&file);
    let start = Instant::now();
    let outcome = capture_master(
        &file,
        child,
        Duration::from_secs(10),
        Duration::from_secs(1),
        CAPTURE_CAP,
        |_| false,
    );
    assert_eq!(outcome.end, CaptureEnd::GraceExpired);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "grace expiry stays inside the budget"
    );
    drop(slave);
    drop(file);
}

#[test]
fn capture_cap_reports_truncation_never_complete() {
    // Byte cap: overflow truncates, flags it, and the retained
    // buffer is exactly the cap. Marker matching covers the
    // retained bytes; truncated evidence is never complete.
    let (read, write) = nix::unistd::pipe().expect("pipe");
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            drop(write);
            reap_child(child);
            let file = std::fs::File::from(read);
            set_nonblocking(&file);
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(2),
                64,
                |_| false,
            );
            assert_eq!(outcome.end, CaptureEnd::CapHit);
            assert!(outcome.truncated, "cap reports truncation");
            assert_eq!(outcome.bytes.len(), 64);
            assert_eq!(&outcome.bytes[..6], b"000000");
        }
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::close(read.into_raw_fd());
            use std::os::unix::io::{AsFd, AsRawFd};
            let w = write.as_fd().as_raw_fd();
            std::mem::forget(write);
            let chunk = [b'0'; 256];
            let wrote = unsafe {
                libc::write(
                    w,
                    chunk.as_ptr() as *const std::os::raw::c_void,
                    chunk.len() as libc::size_t,
                )
            };
            if wrote != 256 {
                unsafe { libc::_exit(2) };
            }
            unsafe { libc::_exit(0) };
        }
    }
}

/// Sets O_NONBLOCK on a master fd (controls only): the
/// collector contract requires it, and a blocking read would
/// sit past the bound on a silent child.
pub fn set_nonblocking(file: &std::fs::File) {
    use std::os::unix::io::AsRawFd;
    let flags =
        nix::fcntl::fcntl(file.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).expect("flags query");
    let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
    oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(file.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags))
        .expect("nonblocking");
}

#[test]
fn excerpt_caps_long_buffers_and_marks_omission() {
    // Display bound: long buffers excerpt head+tail with an
    // omission count; short buffers render whole. Control
    // characters escape, never raw.
    let bytes = vec![b'A'; 3000];
    let shown = excerpt_capped(&bytes);
    assert!(shown.len() < bytes.len(), "excerpt is bounded");
    assert!(shown.contains("omitted"), "omission marked");
    assert!(shown.starts_with(&"A".repeat(512)), "head retained");
    assert!(shown.ends_with(&"A".repeat(512)), "tail retained");
    assert_eq!(excerpt_capped(b"ok\n"), "ok\n");
    assert_eq!(excerpt_capped(b"\x1b[?6c"), "<ESC>[?6c");
}

#[test]
fn refusal_hits_reports_occurrences_not_verdicts() {
    // Occurrence reporting only: hits name what the bytes
    // contain, and an empty capture proves nothing.
    let hits = refusal_hits(b"launch incarnation unestablishable (refusing)\n");
    assert_eq!(hits, vec!["unestablishable"]);
    let hits = refusal_hits(b"error: selector: no session matches; stale data\n");
    assert_eq!(hits, vec!["stale", "selector: no session matches"]);
    assert!(
        refusal_hits(b"clean output\n").is_empty(),
        "no markers, no hits"
    );
    assert!(refusal_hits(b"").is_empty(), "empty capture proves nothing");
}

#[test]
fn capture_drains_after_exit_before_first_read() {
    // Post-exit drain converges with complete bytes: the child
    // writes and exits before the parent reads anything (the
    // parent reaps first, so exit strictly precedes the first
    // read), and the collector still drains to EOF. (The
    // retired stop-on-exit shape is pinned separately below:
    // it breaks on the observed exit and loses bytes written
    // inside the read-then-poll window.)
    let expected = b"COMPLETE-BYNARY-MARKER\n";
    let (read, write) = nix::unistd::pipe().expect("pipe");
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            drop(write);
            reap_child(child);
            let file = std::fs::File::from(read);
            set_nonblocking(&file);
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(2),
                CAPTURE_CAP,
                |_| false,
            );
            assert_eq!(outcome.bytes, expected, "post-exit drain keeps every byte");
            assert_eq!(outcome.end, CaptureEnd::Eof);
            assert_eq!(
                outcome.disposition,
                ChildDisposition::Unavailable,
                "pre-reaped child reads as reaped-elsewhere, not an exit code"
            );
            assert!(outcome.reads >= 1, "at least one read delivered");
            assert!(outcome.child_reaped, "reaped before draining");
            assert!(outcome.phases.first_read.is_some(), "read observed");
            assert!(
                outcome.phases.exit_observed.is_some(),
                "child-status observation recorded"
            );
            assert!(
                outcome.phases.ended >= outcome.phases.started,
                "monotonic phases"
            );
            assert!(
                outcome.refusal_hits.is_empty(),
                "no refusal vocabulary in marker"
            );
        }
        nix::unistd::ForkResult::Child => {
            use std::os::unix::io::{AsFd, AsRawFd};
            let w = write.as_fd().as_raw_fd();
            std::mem::forget(write);
            let _ = nix::unistd::close(read.into_raw_fd());
            let mut written = 0;
            while written < expected.len() {
                let wrote = unsafe {
                    libc::write(
                        w,
                        expected[written..].as_ptr() as *const std::os::raw::c_void,
                        (expected.len() - written) as libc::size_t,
                    )
                };
                if wrote <= 0 {
                    unsafe { libc::_exit(2) };
                }
                written += wrote as usize;
            }
            unsafe { libc::_exit(0) };
        }
    }
}

#[test]
fn capture_unreadable_fd_reports_read_error() {
    // Deterministic read failure: reading a directory fd
    // fails EISDIR on Linux — the only way to produce
    // ReadErr here, so the end-state classification F3 hinges
    // on is pinned, not merely present. (A Ws counterpart
    // cannot exist: WouldBlock never terminates a capture.)
    let dir = tempfile::tempdir().expect("tmpdir");
    let file = std::fs::File::open(dir.path()).expect("open dir reads");
    set_nonblocking(&file);
    let child = match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => child,
        nix::unistd::ForkResult::Child => unsafe { libc::_exit(0) },
    };
    let outcome = capture_master(
        &file,
        child,
        Duration::from_secs(10),
        Duration::from_secs(2),
        CAPTURE_CAP,
        |_| false,
    );
    reap_child(child);
    match &outcome.end {
        CaptureEnd::ReadErr { errno, .. } => assert_eq!(
            *errno,
            Some(libc::EISDIR),
            "directory read fails with typed EISDIR, got end: {:?}",
            outcome.end
        ),
        other => panic!("directory read fails as ReadErr, got: {other:?}"),
    }
    assert!(outcome.bytes.is_empty(), "failed read captures nothing");
}

#[test]
fn capture_split_writes_reassemble() {
    // Three chunks, one EOF: the retained buffer is the
    // concatenation, and the marker search runs over all of
    // it, not over a display excerpt.
    let chunks: [&[u8]; 3] = [b"FIRST-", b"SECOND-", b"THIRD"];
    let (read, write) = nix::unistd::pipe().expect("pipe");
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            drop(write);
            reap_child(child);
            let file = std::fs::File::from(read);
            set_nonblocking(&file);
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(2),
                CAPTURE_CAP,
                |bytes| String::from_utf8_lossy(bytes).contains("THIRD"),
            );
            assert_eq!(outcome.bytes, b"FIRST-SECOND-THIRD");
            assert!(
                outcome.matched,
                "settle predicate fires over retained bytes"
            );
            assert_eq!(outcome.end, CaptureEnd::Eof);
            assert!(outcome.refusal_hits.is_empty(), "no refusal vocabulary");
        }
        nix::unistd::ForkResult::Child => {
            use std::os::unix::io::{AsFd, AsRawFd};
            let w = write.as_fd().as_raw_fd();
            std::mem::forget(write);
            let _ = nix::unistd::close(read.into_raw_fd());
            for chunk in chunks {
                let wrote = unsafe {
                    libc::write(
                        w,
                        chunk.as_ptr() as *const std::os::raw::c_void,
                        chunk.len() as libc::size_t,
                    )
                };
                if wrote != chunk.len() as isize {
                    unsafe { libc::_exit(2) };
                }
            }
            unsafe { libc::_exit(0) };
        }
    }
}

#[test]
fn capture_silent_nonzero_exit_is_empty_eof() {
    // Silent failure is empty EOF, not a hang and not an
    // error: the exit code is the caller's business (verdict
    // rules); the capture only reports what arrived.
    let (read, write) = nix::unistd::pipe().expect("pipe");
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            drop(write);
            reap_child(child);
            let file = std::fs::File::from(read);
            set_nonblocking(&file);
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(2),
                CAPTURE_CAP,
                |_| false,
            );
            assert!(
                outcome.bytes.is_empty(),
                "nothing written, nothing captured"
            );
            assert_eq!(outcome.end, CaptureEnd::Eof);
        }
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::close(read.into_raw_fd());
            let _ = nix::unistd::close(write.into_raw_fd());
            unsafe { libc::_exit(1) };
        }
    }
}

/// Retired stop-on-exit read loop: a faithful relevant
/// fragment of the retired production shape — read first,
/// then break on an observed exit, never draining past it.
/// Test-only differences from retired production, stated:
/// bounded 10ms convergence polls instead of the old sleeps,
/// a hook slot fired once after the first WouldBlock (used
/// below to force the implicated window deterministically),
/// and an ECHILD break (a reaped-elsewhere pid is an observed
/// exit too). The read-then-poll-then-break order itself is
/// the shape under test. Kept ONLY as the failing half of the
/// comparison; never use this shape.
fn retired_stop_on_exit(
    master: &std::fs::File,
    child: nix::unistd::Pid,
    bound: Duration,
    on_first_wouldblock: Option<&dyn Fn()>,
) -> (Vec<u8>, &'static str) {
    let mut fd = master;
    let mut buf = [0u8; 4096];
    let mut acc = Vec::new();
    let mut hook = on_first_wouldblock;
    let start = Instant::now();
    loop {
        if start.elapsed() > bound {
            return (acc, "timeout");
        }
        match std::io::Read::read(&mut fd, &mut buf) {
            Ok(0) => return (acc, "eof"),
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // The implicated window opens here: the hook
                // releases the waiting child and reaps its
                // exit (bounded, fail-loud) before the old poll
                // below runs, so the write+exit land between
                // this read and that poll deterministically —
                // scheduled, never raced.
                if let Some(release) = hook.take() {
                    release();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return (acc, "read-error"),
        }
        match nix::sys::wait::waitpid(child, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
            Ok(nix::sys::wait::WaitStatus::StillAlive) => {}
            Ok(_) => return (acc, "saw-exit"),
            Err(nix::errno::Errno::ECHILD) => return (acc, "saw-exit-echild"),
            Err(_) => {}
        }
    }
}

/// Close every fd except `keep` (forked children only, raw
/// libc — safe post-fork): a gate-blocked child must not hold
/// sibling tests' pipe ends open across parallel tests, or
/// their EOF never arrives and they time out. This includes
/// stdio: the forkpty early-close child closes its slave this
/// way. Call after `into_raw_fd`, before any blocking wait.
pub fn close_unrelated_fds(keep: &[std::os::unix::io::RawFd]) {
    for fd in 0..8192 {
        if keep.contains(&fd) {
            continue;
        }
        unsafe {
            libc::close(fd);
        }
    }
}

/// Bounded gate wait for forked children (libc only): waits up
/// to `bound` for one release byte on `fd`. True when
/// released; false on timeout or error — the caller must exit
/// distinctly and never proceed as if released.
pub fn gate_wait(fd: std::os::unix::io::RawFd, bound: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN as _,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pfd, 1, bound.as_millis() as _) };
    if ready <= 0 {
        return false;
    }
    let mut one = [0u8; 1];
    unsafe { libc::read(fd, one.as_mut_ptr() as *mut _, 1) == 1 }
}

#[test]
fn capture_old_shape_loses_bytes_new_shape_retains_them() {
    // The implicated interleaving, deterministically forced on
    // the SAME direct child (no descendants): the child blocks
    // on a gate before writing; the parent's first WouldBlock
    // releases it and reaps its exit before the old poll runs,
    // so the write+exit land inside the read-then-poll window.
    // The retired shape breaks with an empty buffer; a second,
    // identically-choreographed direct child proves the
    // collector retains the bytes. Every step is gated
    // (release, reap, EOF-terminated drain) or bounded with
    // fail-loud expiry — no timing, no orphans: both children
    // are owned and reaped, gate waits are bounded with
    // distinct exit codes, and unused ends are closed (never
    // forgotten) so no failure path can hang.
    for new_shape in [false, true] {
        let (read, write) = nix::unistd::pipe().expect("pipe");
        let (gate_read, gate_write) = nix::unistd::pipe().expect("pipe");
        match unsafe { nix::unistd::fork().expect("fork") } {
            nix::unistd::ForkResult::Parent { child } => {
                drop(write);
                drop(gate_read);
                let file = std::fs::File::from(read);
                set_nonblocking(&file);
                if !new_shape {
                    // The hook schedules the loss window: release
                    // the child, then reap its exit (bounded)
                    // before the old poll below observes it.
                    let release_and_reap = || {
                        let wrote = nix::unistd::write(&gate_write, &[1u8]);
                        assert!(wrote.is_ok(), "gate release delivers");
                        let mut reaped = false;
                        let mut exited_at = None;
                        let mut disposition = ChildDisposition::Unknown;
                        spin_until_gone(child, &mut reaped, &mut exited_at, &mut disposition);
                        assert!(reaped, "released child converges");
                    };
                    let (old_bytes, old_why) = retired_stop_on_exit(
                        &file,
                        child,
                        Duration::from_secs(10),
                        Some(&release_and_reap),
                    );
                    assert!(
                        old_bytes.is_empty(),
                        "retired shape breaks before the gated write lands"
                    );
                    assert_eq!(
                        old_why, "saw-exit-echild",
                        "retired shape breaks on the reaped exit"
                    );
                    drop(gate_write);
                } else {
                    // Same choreography, collector under test:
                    // release, then drain to EOF. The collector
                    // has no exit-break, so every schedule
                    // retains; the gate only orders the write,
                    // never the verdict.
                    let wrote = nix::unistd::write(&gate_write, &[1u8]);
                    assert!(wrote.is_ok(), "gate release delivers");
                    drop(gate_write);
                    let outcome = capture_master(
                        &file,
                        child,
                        Duration::from_secs(10),
                        Duration::from_secs(2),
                        CAPTURE_CAP,
                        |bytes| String::from_utf8_lossy(bytes).contains("PART2"),
                    );
                    assert_eq!(
                        outcome.bytes, b"PART2\n",
                        "new shape retains the gated bytes"
                    );
                    assert_eq!(outcome.end, CaptureEnd::Eof);
                    assert!(outcome.matched, "settle predicate fires on the gated bytes");
                    let fin = finalize_capture(
                        child,
                        "old-vs-new",
                        &outcome,
                        Instant::now() + Duration::from_secs(10),
                    );
                    assert_eq!(fin.disposition, ChildDisposition::Exited(0));
                    let end_ok = matches!(outcome.end, CaptureEnd::Eof | CaptureEnd::PtyEio { .. });
                    assert!(
                        verdict_ready(end_ok, &fin, outcome.matched),
                        "clean EOF plus clean exit verifies"
                    );
                }
            }
            nix::unistd::ForkResult::Child => {
                unsafe { libc::close(gate_write.into_raw_fd()) };
                let gate = gate_read.into_raw_fd();
                unsafe { libc::close(read.into_raw_fd()) };
                let w = write.into_raw_fd();
                // A gate-blocked child outlives the capture
                // window: shed every inherited sibling fd first
                // so this block cannot hold another test's EOF
                // open.
                close_unrelated_fds(&[w, gate]);
                // Wait for the release (bounded: a dead parent
                // surfaces as exit 2, never a hang and never a
                // false pass — the parent asserts convergence).
                if !gate_wait(gate, Duration::from_secs(15)) {
                    unsafe { libc::_exit(2) };
                }
                unsafe { libc::close(gate) };
                let message = b"PART2\n";
                let wrote = unsafe {
                    libc::write(
                        w,
                        message.as_ptr() as *const std::os::raw::c_void,
                        message.len() as libc::size_t,
                    )
                };
                unsafe { libc::close(w) };
                if wrote != message.len() as isize {
                    unsafe { libc::_exit(2) };
                }
                unsafe { libc::_exit(0) };
            }
        }
    }
}
