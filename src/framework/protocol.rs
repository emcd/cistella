//! Versioned stdio protocol host.
//!
//! Length-prefixed framing, hello/negotiation, request-ID correlation
//! with the narrow `await_result` streaming exception, bounded
//! responses, deadline-owned kill semantics, and reverse-order
//! shutdown with residue-dominated reporting.
//!
//! Wire shape: `4-byte unsigned big-endian frame length` followed by
//! exactly that many UTF-8 JSON envelope bytes (no header, no
//! trailer). Envelopes are `{protocol, id, op, payload}` objects;
//! unknown fields refuse deterministically. The first hello frame
//! arrives before negotiation, so an independent 64 KiB ceiling
//! applies to it regardless of the negotiated maximum.
//!
//! Two layers: [`Exchange`] speaks frames over any `Read + AsFd` /
//! `Write` pair (unit-testable over socketpairs, no child required);
//! [`GuestHost`] spawns a pinned executable and owns its lifetime
//! (process-group kill, reap, bounded stderr drain, SIGPIPE guard).

#[path = "protocol_exchange.rs"]
mod protocol_exchange;
#[path = "protocol_frames.rs"]
mod protocol_frames;

use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};

use nix::sys::select::{FdSet, select};
use nix::sys::time::TimeVal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CistellaError, Result};

pub use crate::framework::guest::{GuestHost, StderrDrain};

pub use self::protocol_exchange::Exchange;
pub use self::protocol_frames::{AWAIT_RESULT_OP, read_frame, write_frame};
pub(crate) use self::protocol_frames::{FrameAssembler, FramePoll};

/// Protocol major version this host speaks.
pub const PROTOCOL_MAJOR: u32 = 2;

/// Hello capability gating the typed `Unobservable` await variant.
/// The guest emits it ONLY when the host offered it at hello;
/// otherwise the backend outcome downgrades to the legacy raw
/// mapping. Emission without negotiation refuses host-side.
pub const AWAIT_INTERRUPTED_V1: &str = "await-interrupted-v1";

/// True when a hello payload's capability list names `capability.
/// Pure so guest hello handling and unit pins share the check.
#[must_use]
pub fn offered_capability(payload: &serde_json::Value, capability: &str) -> bool {
    payload
        .get("capabilities")
        .and_then(|capabilities| capabilities.as_array())
        .is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|name| name.as_str() == Some(capability))
        })
}

/// Pre-negotiation frame ceiling: applies to the first hello frame
/// regardless of the negotiated maximum.
pub const PRE_NEGOTIATION_MAX_FRAME: usize = 64 * 1024;

/// Default negotiated maximum frame when the guest names none.
pub const DEFAULT_MAX_FRAME: usize = 1024 * 1024;

/// Hard host ceiling: negotiated maxima above this are clamped.
pub const HOST_MAX_FRAME: usize = 8 * 1024 * 1024;

/// Stderr accounting cap: bytes beyond this mark the drain truncated
/// (content is always discarded; only counts survive).
pub const STDERR_CAP: u64 = 1024 * 1024;

/// Bound for the single send inside an otherwise unbounded stream.
const SEND_BUDGET: Duration = Duration::from_secs(10);

/// Read slice for unbounded streams (cancellation stays live).
const READ_POLL_BUDGET: Duration = Duration::from_millis(500);

/// Header size: 4-byte unsigned big-endian frame length.
const HEADER_LEN: usize = 4;

/// Envelope for every frame in both directions.
///
/// Unknown fields refuse deterministically (`deny_unknown_fields`):
/// a guest that invents keys fails the exchange, never negotiates
/// around the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Protocol major version (bootstrap integer).
    pub protocol: u32,
    /// Request correlation ID (requester-chosen, per-connection unique).
    pub id: String,
    /// Operation name (`hello`, `prepare`, `isolator.*`, ...).
    pub op: String,
    /// Operation payload (opaque to framing; validated per op).
    pub payload: Value,
}

/// Host hello payload: version plus offered capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloRequest {
    /// Host protocol major version.
    pub version: u32,
    /// Host capability names.
    pub capabilities: Vec<String>,
}

/// Guest hello payload: version, capabilities, optional max frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloResponse {
    /// Guest protocol major version (must equal host).
    pub version: u32,
    /// Guest capability names.
    pub capabilities: Vec<String>,
    /// Guest-proposed maximum frame (clamped to host ceiling).
    #[serde(default)]
    pub max_frame: Option<u32>,
}

/// Negotiated session: guest capabilities plus enforced maximum.
#[derive(Debug, Clone)]
pub struct Negotiated {
    /// Guest capability names.
    pub capabilities: Vec<String>,
    /// Enforced maximum frame for the rest of the connection.
    pub max_frame: usize,
}

/// Outcome of one streaming exchange: pending count plus terminal payload.
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    /// Number of `{pending: true}` frames before terminal.
    pub pending: u32,
    /// Terminal payload (first non-pending frame).
    pub result: Value,
}

pub(crate) fn protocol_error(message: impl Into<String>) -> CistellaError {
    CistellaError::Protocol(message.into())
}

/// Clean-EOF-at-boundary message: the peer closed an idle
/// connection with zero bytes consumed (not truncation, not a
/// protocol failure). Guests use [`is_clean_eof`] to exit quietly
/// on orderly close while failing loudly on anything else.
pub const EOF_AT_BOUNDARY: &str = "eof at frame boundary";

/// Idle-timeout message: budget exhausted with zero bytes
/// consumed (quiet connection, not failure). Distinct from a
/// timeout after partial bytes, which is a desynchronizing
/// protocol failure — looping again there would drop consumed
/// bytes and misalign the next frame.
pub const IDLE_TIMEOUT: &str = "frame read timed out: idle";

/// Write-timeout message: the write budget exhausted while the
/// peer was not draining (stuck or slow). Partial bytes may
/// already sit in the pipe and neither end resynchronizes, so the
/// dispatcher treats this as fatal: bounded shutdown/reap, then
/// latch and fail. Distinct from the oversize pre-send refusal,
/// which never reaches the peer.
pub const WRITE_TIMEOUT: &str = "frame write timed out";

/// True when the error is a clean EOF at a frame boundary.
///
/// Single source of truth for the boundary message so guests never
/// match error text they do not own.
#[must_use]
pub fn is_clean_eof(error: &CistellaError) -> bool {
    matches!(error, CistellaError::Protocol(message) if message == EOF_AT_BOUNDARY)
}

/// True when the error is a read-budget timeout (idle silence, not
/// failure). Guests loop again on timeouts: quiet control
/// connections are patience, never an exit reason.
#[must_use]
pub fn is_read_timeout(error: &CistellaError) -> bool {
    matches!(error, CistellaError::Protocol(message) if message == IDLE_TIMEOUT)
}

/// True when the error is a frame-write timeout (the peer was not
/// draining within budget). The dispatcher treats this as a fatal
/// exchange failure (quiesce, latch, fail); only the oversize
/// pre-send refusal continues. Exact-match on our own constructor,
/// same pattern as [`is_read_timeout`]: callers never match error
/// text they do not own.
#[must_use]
pub fn is_write_timeout(error: &CistellaError) -> bool {
    matches!(error, CistellaError::Protocol(message) if message == WRITE_TIMEOUT)
}

/// True when a response payload is a non-terminal `{pending: true}`.
///
/// The narrow streaming exception: `await_result`-style exchanges may
/// emit pending frames before exactly one terminal frame. Terminal is
/// anything else.
fn is_pending(payload: &Value) -> bool {
    payload
        .get("pending")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Waits for readability on `fd` until `deadline` (remaining budget).
///
/// Returns true when readable, false on budget exhaustion.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on select failure.
pub(crate) fn wait_readable(fd: &impl AsFd, deadline: Instant) -> Result<bool> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        let remaining = deadline - now;
        let mut timeout =
            TimeVal::new(remaining.as_secs() as i64, remaining.subsec_micros() as i64);
        let raw = fd.as_fd();
        let mut set = FdSet::new();
        set.insert(raw);
        let ready = select(
            raw.as_raw_fd() + 1,
            Some(&mut set),
            None,
            None,
            Some(&mut timeout),
        )
        .map_err(|e| protocol_error(format!("select: {e}")))?;
        if ready > 0 {
            return Ok(true);
        }
    }
}

/// Runs `op` with SIGPIPE blocked on this thread, restoring the
/// mask after.
///
/// Write-side SIGPIPE discipline without process-global state: a
/// dead guest's EPIPE arrives as a typed error on the writing
/// thread, never a signal, and concurrent guests need no shared
/// disposition save/restore. Restore is best-effort; callers keep
/// responsibility for any prior mask they care about.
///
/// Correctness core: with a default disposition, a blocked SIGPIPE
/// stays pending and would kill on unmask AFTER the write observed
/// EPIPE. So while still masked, exactly one NEWLY generated
/// SIGPIPE is consumed (a pre-existing pending SIGPIPE is never
/// touched); mask failures refuse rather than writing unprotected.
/// The single unsafe block groups setup, op, consume, and restore:
/// any libc error returns before the mask is touched, and the
/// restore path is the only place partial state could leak (the
/// caller receives the error). The timed wait matches only the
/// block set, so non-blocked signals (SIGCHLD/SIGTERM) are never
/// consumed — only the SIGPIPE this write may have produced.
fn masked<T>(op: impl FnOnce() -> T) -> Result<T> {
    use nix::libc;
    unsafe {
        let mut before: libc::sigset_t = std::mem::zeroed();
        if libc::sigpending(&mut before) != 0 {
            return Err(protocol_error(format!(
                "sigpending: {}",
                std::io::Error::last_os_error()
            )));
        }
        let had = libc::sigismember(&before, libc::SIGPIPE) == 1;
        let mut block: libc::sigset_t = std::mem::zeroed();
        let mut old: libc::sigset_t = std::mem::zeroed();
        let mask_error = if libc::sigemptyset(&mut block) != 0
            || libc::sigaddset(&mut block, libc::SIGPIPE) != 0
        {
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
        } else {
            // pthread_sigmask returns the errno directly (not via
            // thread-local errno): report the returned code, never a
            // stale last_os_error.
            libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut old)
        };
        if mask_error != 0 {
            return Err(protocol_error(format!(
                "sigmask block: {}",
                std::io::Error::from_raw_os_error(mask_error)
            )));
        }
        let out = op();
        if !had {
            let mut now: libc::sigset_t = std::mem::zeroed();
            if libc::sigpending(&mut now) == 0 && libc::sigismember(&now, libc::SIGPIPE) == 1 {
                // Zero-timeout wait consumes exactly the signal this
                // write generated; EAGAIN (raced away) is harmless.
                let timeout = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::sigtimedwait(&block, &mut info, &timeout);
            }
        }
        let restore_error = libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
        if restore_error != 0 {
            return Err(protocol_error(format!(
                "sigmask restore: {}",
                std::io::Error::from_raw_os_error(restore_error)
            )));
        }
        Ok(out)
    }
}

/// Waits for writability on `fd` until `deadline` (remaining budget).
///
/// Returns true when writable, false on budget exhaustion.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on select failure.
fn wait_writable(fd: &impl AsFd, deadline: Instant) -> Result<bool> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        let remaining = deadline - now;
        let mut timeout =
            TimeVal::new(remaining.as_secs() as i64, remaining.subsec_micros() as i64);
        let raw = fd.as_fd();
        let mut set = FdSet::new();
        set.insert(raw);
        let ready = select(
            raw.as_raw_fd() + 1,
            None,
            Some(&mut set),
            None,
            Some(&mut timeout),
        )
        .map_err(|e| protocol_error(format!("select: {e}")))?;
        if ready > 0 {
            return Ok(true);
        }
    }
}

/// Writes all bytes before `deadline`: a full stdin pipe consumes
/// budget, never an unbounded block.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on budget exhaustion or IO failure.
fn write_all_deadline(
    writer: &mut (impl Write + AsFd),
    mut buf: &[u8],
    deadline: Instant,
) -> Result<()> {
    while !buf.is_empty() {
        if !wait_writable(writer, deadline)? {
            return Err(protocol_error(WRITE_TIMEOUT));
        }
        match masked(|| writer.write(buf)) {
            Ok(Ok(0)) => return Err(protocol_error("frame write: closed pipe")),
            Ok(Ok(n)) => buf = &buf[n..],
            Ok(Err(e))
                if e.kind() == std::io::ErrorKind::Interrupted
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                continue;
            }
            Ok(Err(e)) => return Err(protocol_error(format!("frame write: {e}"))),
            Err(e) => return Err(e),
        }
    }
    match masked(|| writer.flush()) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(protocol_error(format!("frame write: {e}"))),
        Err(e) => Err(e),
    }
}

/// Writes one length-prefixed frame bounded by `deadline`.
///
/// The host path: every byte the host emits races a budget, so an
/// unreading guest surfaces as a typed timeout instead of a hang.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on oversize, budget exhaustion,
/// or IO failure.
pub fn write_frame_deadline(
    writer: &mut (impl Write + AsFd),
    payload: &[u8],
    max_frame: usize,
    deadline: Instant,
) -> Result<()> {
    if payload.len() > max_frame {
        return Err(protocol_error(format!(
            "frame length {} exceeds maximum {max_frame}",
            payload.len()
        )));
    }
    let header = (payload.len() as u32).to_be_bytes();
    write_all_deadline(writer, &header, deadline)?;
    write_all_deadline(writer, payload, deadline)
}

/// Parses and validates one envelope: UTF-8, JSON object, known
/// fields, grammar-checked op and id.
///
/// Guest-controlled strings never reach diagnostics raw: op/id must
/// match the token grammar, and serde failures map to fixed classes
/// (never echoed content).
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on invalid UTF-8, malformed
/// JSON, shape violations, or grammar violations.
pub fn parse_envelope(body: &[u8]) -> Result<Envelope> {
    let text = std::str::from_utf8(body).map_err(|_| protocol_error("bad envelope: not UTF-8"))?;
    let envelope: Envelope = serde_json::from_str(text).map_err(|e| {
        if e.is_eof() {
            return protocol_error("bad envelope: truncated JSON");
        }
        if e.is_syntax() {
            return protocol_error("bad envelope: malformed JSON");
        }
        protocol_error("bad envelope: shape violation")
    })?;
    check_token(&envelope.op, "op")?;
    check_token(&envelope.id, "id")?;
    Ok(envelope)
}

/// Validates one protocol token (op/id): bounded safe charset.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on empty, overlong, or
/// out-of-grammar tokens.
fn check_token(text: &str, what: &str) -> Result<()> {
    let ok = !text.is_empty()
        && text.len() <= 128
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:/-".contains(c));
    if ok {
        return Ok(());
    }
    Err(protocol_error(format!("bad envelope: {what} shape")))
}

/// Serializes one envelope to frame bytes.
#[must_use]
pub fn envelope_bytes(envelope: &Envelope) -> Vec<u8> {
    serde_json::to_vec(envelope).expect("envelope serializes")
}
