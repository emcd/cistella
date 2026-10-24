//! Bounded PTY/master capture shared by the pty probes.
//!
//! One collector drains a nonblocking master with explicit
//! end-states; verdicts stay at the call sites (facts only
//! here, never PASS/FAIL). Marker matching covers every
//! retained byte; a byte cap reports truncation/overflow and
//! never claims complete evidence. Refusal-keyword hits are
//! observations, never verdicts.
//!
//! Probe contract for end-of-stream admission: EOF and EIO
//! are both admitted as terminal observations. The probes'
//! own children close their ends and never reopen them, so a
//! closed slave (EIO) or drained pipe (EOF) means no further
//! output from this probe; a reopened slave is outside this
//! contract and is not supported. This is a statement about
//! the probes' own behavior, not a kernel guarantee. An
//! early close (before the child is reaped) is recorded as
//! PtyEio with child_reaped unset — admission still requires
//! the final disposition to verify the exit.

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
    /// A read error other than WouldBlock/EIO, with the raw
    /// errno retained (typed, not just the message) so the
    /// classification is checkable.
    ReadErr { message: String, errno: Option<i32> },
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
/// order: first read, settle match, exit observation, stream
/// closure (EOF/EIO, distinct from the status observation),
/// capture end.
pub struct CapturePhases {
    pub started: Instant,
    pub first_read: Option<Instant>,
    pub matched: Option<Instant>,
    pub exit_observed: Option<Instant>,
    pub closed_at: Option<Instant>,
    pub ended: Instant,
}

/// Outcome of one bounded capture: evidence, not a verdict.
pub struct CaptureOutcome {
    pub bytes: Vec<u8>,
    pub reads: u64,
    pub attempts: u64,
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
/// Shared with the deterministic controls, which pin the
/// first-consumption and reaped-elsewhere behavior.
pub fn child_gone(
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
    let mut attempts: u64 = 0;
    let mut matched = false;
    let mut truncated = false;
    let mut child_reaped = false;
    let mut disposition = ChildDisposition::Unknown;
    let mut phases = CapturePhases {
        started: start,
        first_read: None,
        matched: None,
        exit_observed: None,
        closed_at: None,
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
            Ok(0) => {
                attempts += 1;
                phases.closed_at = Some(Instant::now());
                break CaptureEnd::Eof;
            }
            Ok(n) => {
                attempts += 1;
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
                attempts += 1;
                if past_grace {
                    break CaptureEnd::GraceExpired;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                attempts += 1;
                phases.closed_at = Some(Instant::now());
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
            Err(error) => {
                attempts += 1;
                break CaptureEnd::ReadErr {
                    message: error.to_string(),
                    errno: error.raw_os_error(),
                };
            }
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
        attempts,
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
/// observation, stream closure, and capture end. Missing
/// observations render as unset, never as zero.
pub fn phase_offsets(phases: &CapturePhases) -> String {
    format!(
        "first_read={} matched={} exit={} closed={} end=+{}ms",
        ms_since(phases.started, phases.first_read),
        ms_since(phases.started, phases.matched),
        ms_since(phases.started, phases.exit_observed),
        ms_since(phases.started, phases.closed_at),
        phases.ended.duration_since(phases.started).as_millis(),
    )
}

/// Milliseconds-since-start rendering shared by phase and
/// finalization diagnostics. Missing observations render as
/// unset, never as zero.
pub fn ms_since(start: Instant, point: Option<Instant>) -> String {
    match point {
        Some(instant) => format!("+{}ms", instant.duration_since(start).as_millis()),
        None => "unset".to_string(),
    }
}

/// Finalization: the bounded final reap after capture,
/// carrying the first actually consumed disposition into the
/// verdict. Capture-time observation wins (first consumption);
/// a still-unreaped child is waited on boundedly, then forced
/// with SIGKILL (a kill that never reaps is unproven, never
/// success). Facts only — the call site decides the verdict.
pub struct Finalization {
    pub reaped: bool,
    pub disposition: ChildDisposition,
    pub forced_kill: bool,
    pub exit_observed: Option<Instant>,
}

/// Bounded final reap for one capture. The outcome's
/// disposition is the starting point (capture-time first
/// consumption wins); only an Unknown disposition is updated
/// by what this reap observes. A forced kill is recorded,
/// never a success.
pub fn finalize_capture(
    child: nix::unistd::Pid,
    label: &str,
    outcome: &CaptureOutcome,
    deadline: Instant,
) -> Finalization {
    let mut reaped = outcome.child_reaped;
    let mut disposition = outcome.disposition;
    let mut exit_observed = outcome.phases.exit_observed;
    let mut forced_kill = false;
    if !reaped {
        loop {
            if Instant::now() > deadline {
                eprintln!(
                    "{label} child outlived its window; forcing SIGKILL (timeout, not success)"
                );
                let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
                forced_kill = true;
                break;
            }
            if child_gone(child, &mut reaped, &mut exit_observed, &mut disposition) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if forced_kill {
            // Bounded reap after the kill; a kill that never
            // reaps is unproven, never success.
            let reap_end = Instant::now() + Duration::from_secs(10);
            while Instant::now() <= reap_end {
                if child_gone(child, &mut reaped, &mut exit_observed, &mut disposition) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Finalization {
        reaped,
        disposition,
        forced_kill,
        exit_observed,
    }
}

/// Shared verdict core: facts in, no new I/O. The probe
/// supplies its contract — admitted end-states plus the
/// content check — and everything else is uniform: reaped,
/// no forced kill, a successful exit with no unknown or
/// unsuccessful disposition, content present. A capture
/// error, truncation, timeout, or grace expiry never becomes
/// a PASS; neither does an unknown, unsuccessful, or
/// unavailable disposition.
pub fn verdict_ready(end_ok: bool, final_: &Finalization, content_ok: bool) -> bool {
    final_.reaped
        && !final_.forced_kill
        && end_ok
        && matches!(final_.disposition, ChildDisposition::Exited(0))
        && content_ok
}
