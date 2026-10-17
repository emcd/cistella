//! Bounded child-wait and child-read primitives for the test harness.
//!
//! Split from `helpers.rs` (line-count hook): every blocking
//! interaction with a spawned child lives here — id-line reads,
//! run-to-completion with capture, bounded reaps. `helpers.rs`
//! re-exports this module so existing `use super::helpers::*`
//! consumers are unaffected.

use std::fmt;
use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::helpers::{bin, valid_minted_id};

/// Bound on test-body child reaps (post-kill or expected natural
/// exit). The child is the test's own finite script or an already-
/// killed process; the bound exists to fail loudly on a wedged
/// child, never to hurry a healthy one.
pub const CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound on `run_cistella` CLI invocations (seat maxima + headroom;
/// QA confirms on the live seat). Verbs are millisecond-scale; the
/// bound exists to fail loudly on a wedged seat, never to hurry a
/// healthy one.
pub const RUN_CISTELLA_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-stream capture cap for bounded runs (4 MiB). Past the cap the
/// child is killed and the run fails LOUD with overflow — capture is
/// correctness evidence, never silently truncated.
pub const RUN_BOUNDED_STREAM_CAP: usize = 4 * 1024 * 1024;

/// Bound on post-exit pipe draining: the child is dead, so EOF must
/// follow promptly unless a descendant holds the pipe — which
/// surfaces as a labeled drain timeout, never a hang. Public so
/// callers that drain outside `wait_bounded` (ssh guest transcript)
/// compose the same allowance.
pub const POST_EXIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// How a bounded run failed. Timeout carries the partial capture
/// (labeled partial, head-truncated for message size — the run
/// FAILED, so nothing asserts on these bytes; they are triage
/// diagnostics, and the truncation is labeled in the text).
#[derive(Debug)]
pub enum BoundedRunError {
    Timeout {
        waited: Duration,
        reaped: bool,
        partial_stdout: String,
        partial_stderr: String,
    },
    Overflow {
        stream: &'static str,
    },
    DrainTimeout {
        partial_stdout: String,
        partial_stderr: String,
    },
    /// A capture worker hit a read error before EOF: the retained
    /// bytes are partial evidence of unknown completeness and must
    /// never return as `Ok`. The run FAILED; these bytes are triage
    /// diagnostics with labeled truncation.
    Capture {
        stream: &'static str,
        detail: String,
        partial_stdout: String,
        partial_stderr: String,
    },
    /// The wait poll itself failed (not a timeout, not an exit):
    /// the child's disposition is unobtainable, so no outcome —
    /// success or timeout — may be reported. Partials retained
    /// for triage.
    Wait {
        detail: String,
        partial_stdout: String,
        partial_stderr: String,
    },
    Spawn(String),
}

impl fmt::Display for BoundedRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout {
                waited,
                reaped,
                partial_stdout,
                partial_stderr,
            } => write!(
                f,
                "no exit within {}ms (child reaped: {reaped}; partial stdout: {partial_stdout:?}; partial stderr: {partial_stderr:?})",
                waited.as_millis()
            ),
            Self::Overflow { stream } => {
                write!(f, "{stream} exceeded capture cap — killed, not truncated")
            }
            Self::DrainTimeout {
                partial_stdout,
                partial_stderr,
            } => write!(
                f,
                "pipes did not EOF after exit (descendant-held pipe shape; partial stdout: {partial_stdout:?}; partial stderr: {partial_stderr:?})"
            ),
            Self::Capture {
                stream,
                detail,
                partial_stdout,
                partial_stderr,
            } => write!(
                f,
                "{stream} capture failed ({detail}; partial stdout: {partial_stdout:?}; partial stderr: {partial_stderr:?})"
            ),
            Self::Wait {
                detail,
                partial_stdout,
                partial_stderr,
            } => write!(
                f,
                "child wait failed ({detail}; partial stdout: {partial_stdout:?}; partial stderr: {partial_stderr:?})"
            ),
            Self::Spawn(detail) => write!(f, "spawn failed: {detail}"),
        }
    }
}

/// Truncates failure-path bytes for message embedding, with the
/// truncation LABELED (cf. `drain_pipe`'s silent cap, which this
/// replaces on bounded paths). Cuts BYTES first (byte slicing never
/// panics), then lossy-converts: a multibyte codepoint straddling
/// the cut renders as U+FFFD, never panics inside `str` indexing.
fn label_partial(bytes: &[u8]) -> String {
    const SHOW: usize = 8192;
    if bytes.len() <= SHOW {
        return String::from_utf8_lossy(bytes).to_string();
    }
    format!(
        "{}…[truncated, {} bytes total]",
        String::from_utf8_lossy(&bytes[..SHOW]),
        bytes.len()
    )
}

/// Spawns `bin()` for one bounded CLI invocation (HOME + TERM set).
/// Thin entry so `run_cistella` stays a three-line wrapper.
pub fn spawn_cistella_cmd(home: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    cmd.env("HOME", home);
    cmd.env("TERM", "xterm-ghostty");
    cmd
}

/// Bounded run of a piped command: exit within `timeout` (kill +
/// bounded reap past it), full capture (overflow kills LOUD past
/// the cap), pipes EOF within the drain bound past exit. Returns
/// the full `Output` on success — no truncation on the evidence
/// path. Total wall = `timeout` + cleanup allowances (see
/// `wait_bounded`); for a single deadline over everything, use
/// `run_bounded_at`.
pub fn run_bounded(mut cmd: Command, timeout: Duration) -> Result<Output, BoundedRunError> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| BoundedRunError::Spawn(e.to_string()))?;
    wait_bounded(child, timeout)
}

/// Bounded run against ONE absolute deadline (see
/// `wait_bounded_at`): poll loops pass their fixed outer deadline
/// so exit, kill+reap, and drain never stack past it.
pub fn run_bounded_at(mut cmd: Command, deadline: Instant) -> Result<Output, BoundedRunError> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| BoundedRunError::Spawn(e.to_string()))?;
    wait_bounded_at(child, deadline)
}

/// Cleanup allowance past the exit timeout: kill + bounded reap.
/// Part of the documented `wait_bounded` total (exit + reap +
/// drain), never a hidden extension — outer-deadline composers
/// account for it explicitly (see `poll_budget`).
pub const REAP_ALLOWANCE: Duration = Duration::from_secs(10);

/// One stream's capture state. `error` records the FIRST read error
/// (None = clean EOF): a failed capture refuses `Ok` in
/// `finish_capture`, never returns partial bytes as success.
pub struct StreamCapture {
    pub buf: Vec<u8>,
    pub overflow: bool,
    pub error: Option<String>,
}

impl StreamCapture {
    pub fn empty() -> Self {
        Self {
            buf: Vec::new(),
            overflow: false,
            error: None,
        }
    }
}

/// Capped capture worker over any reader: feeds the buffer until
/// EOF, overflow cap, or read error — then reports completion
/// either way. Error and overflow are RECORDED, never swallowed:
/// `finish_capture` refuses both. Shared by `wait_bounded` and the
/// injected-error pin, so the tracking itself is pinned, not
/// merely reviewed.
pub fn drain_capped(
    pipe: impl Read + Send + 'static,
    slot: std::sync::Arc<std::sync::Mutex<StreamCapture>>,
    done: std::sync::mpsc::Sender<()>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let mut guard = slot.lock().expect("capture mutex");
                    if guard.buf.len() + n > RUN_BOUNDED_STREAM_CAP {
                        guard.overflow = true;
                        break;
                    }
                    guard.buf.extend_from_slice(&buf[..n]);
                }
                Err(error) => {
                    slot.lock().expect("capture mutex").error = Some(error.to_string());
                    break;
                }
            }
        }
        let _ = done.send(());
    })
}

/// Bounded drain wait against ONE absolute deadline (shared by
/// `wait_bounded` and the held-writer pin): true = completion
/// observed (handle joinable), false = deadline passed with the
/// pipe still held. Completion is attested by the done channel,
/// never by an unconditional join (finalize precedent).
pub fn wait_drain(rx: &std::sync::mpsc::Receiver<()>, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if rx.recv_timeout(Duration::from_millis(25)).is_ok() {
            return true;
        }
    }
    false
}

/// Post-exit assembly: drain-timeout, capture-error, and overflow
/// checks BEFORE any `Ok`. A failed capture is `Err(Capture)` with
/// the retained bytes as labeled triage — partial evidence of
/// unknown completeness must never read as success. Split from
/// `wait_bounded` so the refusal itself is pinned with poisoned
/// slots (no child needed).
pub fn finish_capture(
    status: std::process::ExitStatus,
    out_slot: &std::sync::Arc<std::sync::Mutex<StreamCapture>>,
    err_slot: &std::sync::Arc<std::sync::Mutex<StreamCapture>>,
    out_ok: bool,
    err_ok: bool,
) -> Result<Output, BoundedRunError> {
    if !(out_ok && err_ok) {
        let out = out_slot.lock().expect("capture mutex");
        let err = err_slot.lock().expect("capture mutex");
        return Err(BoundedRunError::DrainTimeout {
            partial_stdout: label_partial(&out.buf),
            partial_stderr: label_partial(&err.buf),
        });
    }
    let mut out = out_slot.lock().expect("capture mutex");
    let mut err = err_slot.lock().expect("capture mutex");
    let capture_error = out
        .error
        .as_deref()
        .map(|detail| ("stdout", detail.to_string()))
        .or_else(|| {
            err.error
                .as_deref()
                .map(|detail| ("stderr", detail.to_string()))
        });
    if let Some((stream, detail)) = capture_error {
        return Err(BoundedRunError::Capture {
            stream,
            detail,
            partial_stdout: label_partial(&out.buf),
            partial_stderr: label_partial(&err.buf),
        });
    }
    if out.overflow || err.overflow {
        return Err(BoundedRunError::Overflow {
            stream: if out.overflow { "stdout" } else { "stderr" },
        });
    }
    Ok(Output {
        status,
        stdout: std::mem::take(&mut out.buf),
        stderr: std::mem::take(&mut err.buf),
    })
}

/// Bounded wait-and-capture over an already-spawned child. Split
/// from `run_bounded` so stuck-poll pins drive it directly with a
/// `sleep` child (no cistella verb needed).
///
/// Documented total: `timeout` (exit) + `REAP_ALLOWANCE` (kill +
/// reap past expiry) + `POST_EXIT_DRAIN_TIMEOUT` (post-exit pipe
/// EOF). The allowances are named consts with a fixed shape; for
/// a SINGLE deadline governing operation AND cleanup (no stacking
/// past an outer budget), use `wait_bounded_at`.
pub fn wait_bounded(child: Child, timeout: Duration) -> Result<Output, BoundedRunError> {
    wait_bounded_at(
        child,
        Instant::now() + timeout + REAP_ALLOWANCE + POST_EXIT_DRAIN_TIMEOUT,
    )
}

/// Bounded wait-and-capture against ONE absolute deadline: the
/// exit phase, the kill+reap, and the pipe drain ALL complete by
/// `deadline` (plus kill latency, which no waiter can bound).
/// The exit phase ends at `deadline − REAP − DRAIN` so the cleanup
/// allowances provably fit inside the same budget instead of
/// stacking fresh ones past an outer deadline; if the deadline is
/// already inside the reserve, the exit phase is skipped and the
/// child is killed promptly. Reap gets `min(REAP_ALLOWANCE,
/// remaining)` with a 100ms floor (the kernel needs a scheduling
/// quantum past SIGKILL); the drain waits to `deadline` itself.
pub fn wait_bounded_at(mut child: Child, deadline: Instant) -> Result<Output, BoundedRunError> {
    use std::sync::{Arc, Mutex};
    let out_slot = Arc::new(Mutex::new(StreamCapture::empty()));
    let err_slot = Arc::new(Mutex::new(StreamCapture::empty()));
    // A stream the test did not pipe (null/inherit) has no
    // capture: no reader, no wait — `wait_with_output` semantics
    // for caller-chosen stdio, not a silent truncation (the test
    // itself chose not to pipe that stream).
    let out_wait = child.stdout.take().map(|pipe| {
        let (tx, rx) = std::sync::mpsc::channel();
        (drain_capped(pipe, Arc::clone(&out_slot), tx), rx)
    });
    let err_wait = child.stderr.take().map(|pipe| {
        let (tx, rx) = std::sync::mpsc::channel();
        (drain_capped(pipe, Arc::clone(&err_slot), tx), rx)
    });
    // The main loop below checks the slots each iteration: a
    // capped reader stops consuming, so flag the kill promptly
    // instead of waiting out the full timeout against backpressure.
    let start = Instant::now();
    // Single-deadline composition: the exit phase ends early
    // enough that the reap + drain allowances fit INSIDE
    // `deadline`. Saturating: a deadline already inside the
    // reserve kills promptly (see `run_bounded_at` contract —
    // callers must not spawn what cannot clean up).
    let exit_until = deadline
        .checked_sub(REAP_ALLOWANCE + POST_EXIT_DRAIN_TIMEOUT)
        .unwrap_or(start);
    // Reap floor: the kernel needs a scheduling quantum past
    // SIGKILL; totals may exceed `deadline` by up to this on the
    // kill path only, documented, never unbounded.
    let reap_floor = Duration::from_millis(100);
    let reap_budget = || {
        REAP_ALLOWANCE
            .min(deadline.saturating_duration_since(Instant::now()))
            .max(reap_floor)
    };
    let (status, exit_time) = loop {
        {
            let out = out_slot.lock().expect("capture mutex");
            let err = err_slot.lock().expect("capture mutex");
            if out.overflow || err.overflow {
                let _ = child.kill();
                let reaped = reap_bounded(&mut child, reap_budget()).is_some();
                // No assert: an unreaped overflow kill is a loud
                // Timeout (failed cleanup evidence), never a panic
                // — this path runs inside Guard Drop during unwind.
                let stream = if out.overflow { "stdout" } else { "stderr" };
                if !reaped {
                    let out = out_slot.lock().expect("capture mutex");
                    let err = err_slot.lock().expect("capture mutex");
                    return Err(BoundedRunError::Timeout {
                        waited: start.elapsed(),
                        reaped: false,
                        partial_stdout: label_partial(&out.buf),
                        partial_stderr: label_partial(&err.buf),
                    });
                }
                return Err(BoundedRunError::Overflow { stream });
            }
        }
        // try_wait failure is mapped, never panicked: the child's
        // disposition is unobtainable (this path runs inside Guard
        // Drop during unwind, where a panic would abort).
        match child.try_wait() {
            Err(wait_error) => {
                let out = out_slot.lock().expect("capture mutex");
                let err = err_slot.lock().expect("capture mutex");
                return Err(BoundedRunError::Wait {
                    detail: format!("try_wait failed: {wait_error}"),
                    partial_stdout: label_partial(&out.buf),
                    partial_stderr: label_partial(&err.buf),
                });
            }
            Ok(Some(status)) => break (status, Instant::now()),
            Ok(None) => {
                if Instant::now() > exit_until {
                    let _ = child.kill();
                    let reaped = reap_bounded(&mut child, reap_budget()).is_some();
                    let out = out_slot.lock().expect("capture mutex");
                    let err = err_slot.lock().expect("capture mutex");
                    return Err(BoundedRunError::Timeout {
                        waited: start.elapsed(),
                        reaped,
                        partial_stdout: label_partial(&out.buf),
                        partial_stderr: label_partial(&err.buf),
                    });
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };
    // Child reaped: readers must EOF by the SHARED post-exit
    // cap — min(exit time + allowance, absolute deadline) — so a
    // held descendant retains a normal run for the allowance, not
    // the whole outer budget. One cap across both streams.
    // Handles park on timeout (finalize precedent): completion is
    // attested by the done channel, never by an unconditional join.
    let drain_end = exit_time
        .checked_add(POST_EXIT_DRAIN_TIMEOUT)
        .unwrap_or(deadline)
        .min(deadline);
    let out_ok = match out_wait {
        Some((handle, rx)) => {
            let ok = wait_drain(&rx, drain_end);
            if ok {
                let _ = handle.join();
            }
            ok
        }
        None => true,
    };
    let err_ok = match err_wait {
        Some((handle, rx)) => {
            let ok = wait_drain(&rx, drain_end);
            if ok {
                let _ = handle.join();
            }
            ok
        }
        None => true,
    };
    finish_capture(status, &out_slot, &err_slot, out_ok, err_ok)
}

/// How the conduct id-line read failed. Every variant is a loud,
/// diagnosable failure — a silent child or a dead pipe never reads
/// as success, and nonblocking-setup failure surfaces instead of
/// silently degrading to a blocking read.
#[derive(Debug)]
pub enum ConductIdError {
    /// `O_NONBLOCK` setup failed; the os error text is retained.
    Setup(String),
    /// No id line within the bound (silent child shape).
    Timeout { waited: Duration },
    /// EOF before any id line (dead child shape).
    EofWithoutId,
    /// Bytes past the pending cap without an id line (runaway
    /// producer shape).
    LineOverflow { bytes: usize },
    /// A `conduct ` line whose id fails minted-shape validation.
    MalformedId(String),
}

impl fmt::Display for ConductIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(detail) => write!(f, "nonblocking setup failed: {detail}"),
            Self::Timeout { waited } => {
                write!(f, "no id line within {}ms", waited.as_millis())
            }
            Self::EofWithoutId => write!(f, "EOF before any conduct id line"),
            Self::LineOverflow { bytes } => {
                write!(f, "no id line within {bytes} buffered bytes (runaway)")
            }
            Self::MalformedId(line) => write!(f, "malformed conduct id line: {line:?}"),
        }
    }
}

/// Bound on the conduct id-line read (seat maxima + headroom; QA
/// confirms on the live seat). Expiry kills the child — a silent
/// conduct is evidence, never a hang.
pub const CONDUCT_ID_TIMEOUT: Duration = Duration::from_secs(60);

/// Cap on pre-id buffered bytes (64 KiB). An id line arrives
/// within one line; megabytes without a newline is a runaway
/// producer, failed loudly — never an unbounded `pending` buffer.
pub const CONDUCT_ID_PENDING_CAP: usize = 64 * 1024;

/// Reads the `conduct <minted-id>` first line from a child stdout
/// with a hard bound. Nonblocking short-deadline reads: a silent
/// child (bytes never arrive, EOF never arrives) hits `Timeout`,
/// never blocks past the bound. Returns the id, the reader
/// positioned past the id line, plus any bytes read past the
/// id-line newline, so the lifetime drain loses nothing.
///
/// Lines not starting with `conduct ` are skipped (conduct may log
/// to stdout before the id line); a `conduct ` line with a
/// malformed id is `MalformedId`, not a skip.
pub fn read_conduct_id<R: Read + std::os::unix::io::AsRawFd>(
    reader: R,
    timeout: Duration,
) -> Result<(String, R, Vec<u8>), ConductIdError> {
    use std::io::ErrorKind;
    let mut reader = reader;
    if let Ok(flags) = nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL) {
        let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
        oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
        if nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags)).is_err() {
            return Err(ConductIdError::Setup(
                "F_SETFL O_NONBLOCK refused".to_string(),
            ));
        }
    } else {
        return Err(ConductIdError::Setup("F_GETFL refused".to_string()));
    }
    let start = Instant::now();
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // Absolute deadline on EVERY iteration, including
        // successful reads: a continuously-writing newline-free
        // producer must hit Timeout, never spin past the bound.
        if start.elapsed() > timeout {
            return Err(ConductIdError::Timeout {
                waited: start.elapsed(),
            });
        }
        match reader.read(&mut buf) {
            Ok(0) => return Err(ConductIdError::EofWithoutId),
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                if pending.len() > CONDUCT_ID_PENDING_CAP {
                    return Err(ConductIdError::LineOverflow {
                        bytes: pending.len(),
                    });
                }
                while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=pos).collect();
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    if let Some(id) = text.strip_prefix("conduct ") {
                        if valid_minted_id(id) {
                            return Ok((id.to_string(), reader, pending));
                        }
                        return Err(ConductIdError::MalformedId(text));
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

/// Restores blocking mode on a reader that `read_conduct_id` left
/// `O_NONBLOCK`. The lifetime stdout drain is a one-shot blocking
/// `io::copy`: a nonblocking fd returns `WouldBlock` at the first
/// write gap, which ends the copy, drops the reader, closes the
/// pipe, and reintroduces the conduct SIGPIPE/141 failure the
/// drain exists to prevent. `Err` = fcntl refused; the caller must
/// fail loudly, never drain on a nonblocking fd.
pub fn restore_blocking(reader: &impl std::os::unix::io::AsRawFd) -> Result<(), String> {
    let flags = nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)
        .map_err(|e| format!("F_GETFL refused: {e}"))?;
    let oflags = nix::fcntl::OFlag::from_bits_retain(flags) & !nix::fcntl::OFlag::O_NONBLOCK;
    nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags))
        .map_err(|e| format!("F_SETFL blocking restore refused: {e}"))?;
    Ok(())
}

/// Lifetime stdout drain: blocking one-shot copy of the leftover
/// head plus the restored reader into the sink. The reader MUST be
/// blocking (see `restore_blocking`); the sink is `io::sink()` in
/// production and a test-owned pipe in the quiet-gap pin, so the
/// handoff itself is pinned, not just the reader tuple.
pub fn spawn_stdout_drain<R, W>(
    reader: R,
    leftover: Vec<u8>,
    sink: W,
) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
    W: std::io::Write + Send + 'static,
{
    std::thread::spawn(move || {
        let head: &[u8] = &leftover;
        let mut drain = head.chain(reader);
        let mut sink = sink;
        let _ = std::io::copy(&mut drain, &mut sink);
    })
}

/// Bounded reap after a kill: `try_wait` loop against the bound.
/// `Some(status)` = reaped. A SIGKILLed child that will not reap is
/// wedged (D-state); the caller reports loudly — bounded, never
/// hangs. Returns the status (not just a bool) so test bodies
/// migrate their bare `wait()` calls onto this bound without losing
/// exit evidence.
pub fn reap_bounded(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}
