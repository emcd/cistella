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
/// surfaces as a labeled drain timeout, never a hang.
const RUN_BOUNDED_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

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
            Self::Spawn(detail) => write!(f, "spawn failed: {detail}"),
        }
    }
}

/// Truncates failure-path bytes for message embedding, with the
/// truncation LABELED (cf. `drain_pipe`'s silent cap, which this
/// replaces on bounded paths).
fn label_partial(bytes: &[u8]) -> String {
    const SHOW: usize = 8192;
    let text = String::from_utf8_lossy(bytes);
    if bytes.len() > SHOW {
        format!(
            "{}…[truncated, {} bytes total]",
            &text[..SHOW.min(text.len())],
            bytes.len()
        )
    } else {
        text.to_string()
    }
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
/// path.
pub fn run_bounded(mut cmd: Command, timeout: Duration) -> Result<Output, BoundedRunError> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| BoundedRunError::Spawn(e.to_string()))?;
    wait_bounded(child, timeout)
}

/// Bounded wait-and-capture over an already-spawned child. Split
/// from `run_bounded` so stuck-poll pins drive it directly with a
/// `sleep` child (no cistella verb needed).
pub fn wait_bounded(mut child: Child, timeout: Duration) -> Result<Output, BoundedRunError> {
    use std::sync::{Arc, Mutex};
    struct Capture {
        buf: Vec<u8>,
        overflow: bool,
    }
    fn drain_capped(
        pipe: impl Read + Send + 'static,
        slot: Arc<Mutex<Capture>>,
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
                    Err(_) => break,
                }
            }
            let _ = done.send(());
        })
    }
    let out_slot = Arc::new(Mutex::new(Capture {
        buf: Vec::new(),
        overflow: false,
    }));
    let err_slot = Arc::new(Mutex::new(Capture {
        buf: Vec::new(),
        overflow: false,
    }));
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
    let status = loop {
        {
            let out = out_slot.lock().expect("capture mutex");
            let err = err_slot.lock().expect("capture mutex");
            if out.overflow || err.overflow {
                let _ = child.kill();
                let reaped = reap_bounded(&mut child, Duration::from_secs(10)).is_some();
                debug_assert!(reaped, "overflow-killed child reaps");
                let stream = if out.overflow { "stdout" } else { "stderr" };
                return Err(BoundedRunError::Overflow { stream });
            }
        }
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let reaped = reap_bounded(&mut child, Duration::from_secs(10)).is_some();
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
    // Child reaped: readers must EOF within the drain bound (a
    // descendant-held pipe surfaces here, labeled, never hangs).
    // Handles park on timeout (finalize precedent): completion is
    // attested by the done channel, never by an unconditional join.
    let drain_start = Instant::now();
    let drained = |rx: &std::sync::mpsc::Receiver<()>| {
        while drain_start.elapsed() < RUN_BOUNDED_DRAIN_TIMEOUT {
            if rx.recv_timeout(Duration::from_millis(25)).is_ok() {
                return true;
            }
        }
        false
    };
    let out_ok = match out_wait {
        Some((handle, rx)) => {
            let ok = drained(&rx);
            if ok {
                let _ = handle.join();
            }
            ok
        }
        None => true,
    };
    let err_ok = match err_wait {
        Some((handle, rx)) => {
            let ok = drained(&rx);
            if ok {
                let _ = handle.join();
            }
            ok
        }
        None => true,
    };
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
            Self::MalformedId(line) => write!(f, "malformed conduct id line: {line:?}"),
        }
    }
}

/// Bound on the conduct id-line read (seat maxima + headroom; QA
/// confirms on the live seat). Expiry kills the child — a silent
/// conduct is evidence, never a hang.
pub const CONDUCT_ID_TIMEOUT: Duration = Duration::from_secs(60);

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
        match reader.read(&mut buf) {
            Ok(0) => return Err(ConductIdError::EofWithoutId),
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
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
                if start.elapsed() > timeout {
                    return Err(ConductIdError::Timeout {
                        waited: start.elapsed(),
                    });
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(25));
                if start.elapsed() > timeout {
                    return Err(ConductIdError::Timeout {
                        waited: start.elapsed(),
                    });
                }
            }
        }
    }
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
