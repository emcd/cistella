//! Bounded PTY/master capture shared by the pty probes.
//!
//! One collector drains a nonblocking master with explicit
//! end-states; verdicts stay at the call sites (facts only
//! here, never PASS/FAIL). Marker matching covers every
//! retained byte; a byte cap reports truncation/overflow and
//! never claims complete evidence. Refusal-keyword hits are
//! observations, never verdicts.

use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::time::{Duration, Instant};

/// Byte cap for one capture (bounded memory: never an unbounded acc).
pub const CAPTURE_CAP: usize = 1 << 20;

/// Display excerpt bounds: head + tail bytes kept for logs; the
/// marker search always runs over the full retained buffer.
const EXCERPT_HEAD: usize = 512;
const EXCERPT_TAIL: usize = 512;

/// How a bounded capture ended. Facts only — the caller
/// decides the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEnd {
    /// EOF observed (all writers closed).
    Eof,
    /// Linux PTY EIO observed. Whether the child was already
    /// reaped is recorded alongside — EIO with a merely
    /// not-yet-observed child status proves nothing about
    /// liveness; reconcile before concluding. A live child can
    /// close its slave deliberately, so even a genuinely
    /// live-child EIO is an observed early closure, not
    /// automatically a diagnosed pty fault.
    PtyEio { child_reaped: bool },
    /// A read error other than WouldBlock/EIO.
    ReadErr(String),
    /// Byte cap reached (truncation/overflow reported, never
    /// complete evidence).
    CapHit,
    /// Absolute budget exhausted with the stream still open.
    Timeout,
    /// Child gone but the slave is still held (a descendant
    /// holds a writer): the bounded post-exit grace expired
    /// with the stream still open. A distinct
    /// bounded/incomplete capture state, not a clean transcript.
    GraceExpired,
}

/// Monotonic phase timestamps for one capture, in observation
/// order: first read, EIO/EOF observation, child-status
/// observation, marker observation, capture-complete.
pub struct CapturePhases {
    pub started: Instant,
    pub first_read: Option<Instant>,
    pub matched: Option<Instant>,
    pub exit_observed: Option<Instant>,
    pub ended: Instant,
}

/// Outcome of one bounded capture: evidence, not a verdict.
pub struct CaptureOutcome {
    pub bytes: Vec<u8>,
    pub reads: u64,
    pub end: CaptureEnd,
    pub matched: bool,
    pub truncated: bool,
    pub child_reaped: bool,
    pub disposition: ChildDisposition,
    pub phases: CapturePhases,
    pub refusal_hits: Vec<&'static str>,
}

/// Gate-readiness refusal vocabulary observed in captured
/// bytes. Occurrences only — 'stale' may occur in other
/// output, and no hits under incomplete capture never proves
/// absence.
pub fn refusal_hits(bytes: &[u8]) -> Vec<&'static str> {
    const MARKERS: &[&str] = &[
        "no launch attestation",
        "stale",
        "unestablishable",
        "selector: no session matches",
    ];
    let text = String::from_utf8_lossy(bytes);
    MARKERS
        .iter()
        .copied()
        .filter(|marker| text.contains(marker))
        .collect()
}

/// Bounded head+tail excerpt for logs (control characters
/// escaped, never raw): the marker search always runs over
/// the full retained buffer, never this excerpt.
pub fn excerpt_capped(bytes: &[u8]) -> String {
    fn escape(byte: u8) -> String {
        if byte == b'\x1b' {
            "<ESC>".to_string()
        } else if byte.is_ascii_graphic() || byte == b' ' || byte == b'\n' || byte == b'\t' {
            (byte as char).to_string()
        } else {
            format!("<{byte:02X}>")
        }
    }
    if bytes.len() <= EXCERPT_HEAD + EXCERPT_TAIL {
        return bytes.iter().map(|byte| escape(*byte)).collect();
    }
    let head: String = bytes[..EXCERPT_HEAD]
        .iter()
        .map(|byte| escape(*byte))
        .collect();
    let tail: String = bytes[bytes.len() - EXCERPT_TAIL..]
        .iter()
        .map(|byte| escape(*byte))
        .collect();
    let omitted = bytes.len() - EXCERPT_HEAD - EXCERPT_TAIL;
    format!("{head}<...{omitted} bytes omitted...>{tail}")
}

/// How the child ended, as observed. Recorded at first
/// consumption; a later ECHILD means the status was already
/// consumed elsewhere — 'reaped elsewhere / status
/// unavailable', never a successful disposition. Reap
/// bookkeeping (the pid is unwaitable) stays separate from
/// disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildDisposition {
    /// Nothing terminal observed yet.
    Unknown,
    /// Exited with a status code.
    Exited(i32),
    /// Killed by a signal.
    Signaled(i32),
    /// Reaped elsewhere (ECHILD at reconcile): gone, but the
    /// disposition is unknown and satisfies no required
    /// successful disposition.
    Unavailable,
}

/// Reconciles one child status: true when no exit is left to
/// await. A terminal waitpid observation reaps and records
/// the disposition at first consumption; ECHILD means reaped
/// elsewhere (recorded as reaped, disposition Unavailable);
/// any other error is transient and concludes nothing.
fn child_gone(
    child: nix::unistd::Pid,
    reaped: &mut bool,
    exited_at: &mut Option<Instant>,
    disposition: &mut ChildDisposition,
) -> bool {
    match nix::sys::wait::waitpid(child, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
        Ok(nix::sys::wait::WaitStatus::StillAlive) => false,
        Ok(status) => {
            // Only terminal observations converge: a WNOHANG
            // poll without stop flags never reports stopped or
            // continued states, but the match stays total.
            let terminal = matches!(
                status,
                nix::sys::wait::WaitStatus::Exited(_, _)
                    | nix::sys::wait::WaitStatus::Signaled(_, _, _)
            );
            if terminal {
                *reaped = true;
                if matches!(disposition, ChildDisposition::Unknown) {
                    *disposition = match status {
                        nix::sys::wait::WaitStatus::Exited(_, code) => {
                            ChildDisposition::Exited(code)
                        }
                        nix::sys::wait::WaitStatus::Signaled(_, signal, _) => {
                            ChildDisposition::Signaled(signal as i32)
                        }
                        _ => ChildDisposition::Unknown,
                    };
                }
                if exited_at.is_none() {
                    *exited_at = Some(Instant::now());
                }
            }
            terminal
        }
        Err(nix::errno::Errno::ECHILD) => {
            *reaped = true;
            if matches!(disposition, ChildDisposition::Unknown) {
                *disposition = ChildDisposition::Unavailable;
            }
            if exited_at.is_none() {
                *exited_at = Some(Instant::now());
            }
            true
        }
        Err(_) => false,
    }
}

/// Drains one nonblocking master (`O_NONBLOCK` setup stays at
/// the call site) until EOF, EIO, error, cap, budget, or
/// post-exit grace expiry. Child exit never stops the drain;
/// the bounded grace (inside the absolute budget) bounds a
/// descendant-held slave instead of hanging on it. The
/// settle predicate runs over the retained buffer after every
/// read; a match is stamped, never a break — capture runs to
/// a terminal observation.
pub fn capture_master(
    master: &std::fs::File,
    child: nix::unistd::Pid,
    budget: Duration,
    grace: Duration,
    cap: usize,
    done: impl Fn(&[u8]) -> bool,
) -> CaptureOutcome {
    let start = Instant::now();
    let deadline = start + budget;
    let mut bytes = Vec::new();
    let mut reads: u64 = 0;
    let mut matched = false;
    let mut truncated = false;
    let mut child_reaped = false;
    let mut disposition = ChildDisposition::Unknown;
    let mut phases = CapturePhases {
        started: start,
        first_read: None,
        matched: None,
        exit_observed: None,
        ended: start,
    };
    let mut buf = [0u8; 4096];
    let mut fd_read = master;
    let end = loop {
        // The absolute deadline is authoritative: an overrun
        // is Timeout, never GraceExpired, and the grace never
        // extends the budget.
        if Instant::now() > deadline {
            break CaptureEnd::Timeout;
        }
        // Grace check before the read, so the grace boundary
        // still takes one last read while inside the budget.
        let past_grace = match phases.exit_observed {
            Some(exited_at) => Instant::now() > exited_at + grace,
            None => false,
        };
        match std::io::Read::read(&mut fd_read, &mut buf) {
            Ok(0) => break CaptureEnd::Eof,
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                reads += 1;
                if phases.first_read.is_none() {
                    phases.first_read = Some(Instant::now());
                }
                if bytes.len() > cap {
                    bytes.truncate(cap);
                    truncated = true;
                    break CaptureEnd::CapHit;
                }
                if !matched && done(&bytes) {
                    matched = true;
                    phases.matched = Some(Instant::now());
                }
                if past_grace {
                    break CaptureEnd::GraceExpired;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if past_grace {
                    break CaptureEnd::GraceExpired;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                // Reconcile at the EIO instant: EIO with a
                // merely not-yet-observed status proves nothing
                // about liveness.
                child_gone(
                    child,
                    &mut child_reaped,
                    &mut phases.exit_observed,
                    &mut disposition,
                );
                break CaptureEnd::PtyEio { child_reaped };
            }
            Err(error) => break CaptureEnd::ReadErr(error.to_string()),
        }
        // Reconcile child status every iteration (not only on
        // EIO): the record keeps read observation and
        // child-status observation in order.
        child_gone(
            child,
            &mut child_reaped,
            &mut phases.exit_observed,
            &mut disposition,
        );
    };
    phases.ended = Instant::now();
    let refusal_hits = refusal_hits(&bytes);
    CaptureOutcome {
        bytes,
        reads,
        end,
        matched,
        truncated,
        child_reaped,
        disposition,
        phases,
        refusal_hits,
    }
}

/// Phase offsets in milliseconds from capture start, for the
/// diagnostic record: first read, settle match, exit
/// observation, and capture end. Missing observations render
/// as unset, never as zero.
pub fn phase_offsets(phases: &CapturePhases) -> String {
    fn at(start: Instant, point: Option<Instant>) -> String {
        match point {
            Some(instant) => format!("+{}ms", instant.duration_since(start).as_millis()),
            None => "unset".to_string(),
        }
    }
    format!(
        "first_read={} matched={} exit={} end=+{}ms",
        at(phases.started, phases.first_read),
        at(phases.started, phases.matched),
        at(phases.started, phases.exit_observed),
        phases.ended.duration_since(phases.started).as_millis(),
    )
}

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
            use std::os::unix::io::AsFd;
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
fn set_nonblocking(file: &std::fs::File) {
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
    // The losing interleaving, deterministically: the child
    // writes and exits before the parent reads anything (the
    // parent reaps first, so exit strictly precedes the first
    // read). The old shape — break on observed exit — yields
    // empty here; the collector must drain to EOF instead.
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
            use std::os::unix::io::AsFd;
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
            use std::os::unix::io::AsFd;
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
