//! Versioned stdio protocol host (task 2.1).
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

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};

use nix::sys::select::{FdSet, select};
use nix::sys::time::TimeVal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CistellaError, Result};
use crate::framework::contract::CancelFlag;

pub use crate::framework::guest::{GuestHost, StderrDrain};

/// Protocol major version this host speaks.
pub const PROTOCOL_MAJOR: u32 = 1;

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

/// Reads one length-prefixed frame, rejecting oversize before allocation.
///
/// The 4-byte big-endian length counts exactly the envelope bytes. A
/// declared length above `max_frame` refuses without allocating the
/// payload; truncation or EOF mid-frame fails the exchange.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on oversize, timeout, truncation,
/// or IO failure.
pub fn read_frame(
    reader: &mut (impl Read + AsFd),
    max_frame: usize,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut assembler = FrameAssembler::new();
    loop {
        // Bounded reads never idle and never trickle forever. Past
        // the deadline, zero consumed bytes is idle silence
        // (loopable); any consumed bytes is a stalled trickle that
        // already desynchronized the stream (failure).
        if Instant::now() >= deadline {
            return Err(protocol_error(if assembler.is_fresh() {
                IDLE_TIMEOUT
            } else {
                "frame read timed out"
            }));
        }
        match assembler.poll_once(reader, max_frame, deadline)? {
            FramePoll::Complete(body) => return Ok(body),
            FramePoll::Idle => {
                return Err(protocol_error(IDLE_TIMEOUT));
            }
            FramePoll::Partial => continue,
        }
    }
}

/// Incremental frame assembly across poll slices.
///
/// Unbounded waits must distinguish idle (no bytes yet — keep
/// polling, cancellation stays live) from mid-frame progress (bytes
/// consumed — parsing must NOT restart, or the stream corrupts).
/// The assembler keeps partial state across `poll_once` calls; a
/// bounded `read_frame` treats any idle slice as timeout.
pub(crate) struct FrameAssembler {
    header: [u8; HEADER_LEN],
    header_read: usize,
    length: Option<usize>,
    body: Vec<u8>,
}

/// One poll outcome: complete frame, idle slice, or kept progress.
pub(crate) enum FramePoll {
    /// Complete frame bytes.
    Complete(Vec<u8>),
    /// Budget exhausted with zero new bytes (idle, not failure).
    Idle,
    /// Bytes consumed, frame incomplete (state kept, poll again).
    Partial,
}

impl FrameAssembler {
    pub(crate) fn new() -> Self {
        Self {
            header: [0u8; HEADER_LEN],
            header_read: 0,
            length: None,
            body: Vec::new(),
        }
    }

    /// True when zero bytes have been consumed (fresh frame start).
    pub(crate) fn is_fresh(&self) -> bool {
        self.header_read == 0 && self.length.is_none() && self.body.is_empty()
    }

    /// Polls once toward a complete frame.
    ///
    /// Two callers, two lifetimes: bounded `read_frame` owns one
    /// assembler per call (any non-complete slice ends the read),
    /// while unbounded `collect_unbounded` owns one assembler across
    /// the whole stream (idle/partial slices poll again with state
    /// kept). The assembler itself is lifetime-agnostic.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on oversize (refused before
    /// allocating the body), EOF/truncation mid-frame, or IO failure.
    /// Budget exhaustion surfaces as `Idle`/`Partial`, never an error.
    pub(crate) fn poll_once(
        &mut self,
        reader: &mut (impl Read + AsFd),
        max_frame: usize,
        deadline: Instant,
    ) -> Result<FramePoll> {
        // Header first (restartable only while zero bytes consumed).
        while self.header_read < HEADER_LEN {
            if !wait_readable(reader, deadline)? {
                return Ok(if self.header_read == 0 && self.body.is_empty() {
                    FramePoll::Idle
                } else {
                    FramePoll::Partial
                });
            }
            match reader.read(&mut self.header[self.header_read..]) {
                // Zero bytes with zero header consumed is a clean EOF
                // at a frame boundary (guest/host closed an idle
                // connection), distinct from mid-frame truncation.
                Ok(0) if self.header_read == 0 && self.body.is_empty() => {
                    return Err(protocol_error(EOF_AT_BOUNDARY));
                }
                Ok(0) => return Err(protocol_error("truncated frame: EOF mid-frame")),
                Ok(n) => self.header_read += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(protocol_error(format!("frame read: {e}"))),
            }
        }
        let length = match self.length {
            Some(length) => length,
            None => {
                let length = u32::from_be_bytes(self.header) as usize;
                if length > max_frame {
                    return Err(protocol_error(format!(
                        "frame length {length} exceeds maximum {max_frame}"
                    )));
                }
                self.length = Some(length);
                self.body = Vec::with_capacity(length.min(65536));
                length
            }
        };
        while self.body.len() < length {
            if !wait_readable(reader, deadline)? {
                return Ok(FramePoll::Partial);
            }
            let remaining = length - self.body.len();
            let mut chunk = vec![0u8; remaining.min(8192)];
            match reader.read(&mut chunk) {
                Ok(0) => return Err(protocol_error("truncated frame: EOF mid-frame")),
                Ok(n) => self.body.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(protocol_error(format!("frame read: {e}"))),
            }
        }
        // Reset for the next frame on this connection: a stale header
        // or length would corrupt every subsequent parse.
        self.header_read = 0;
        self.length = None;
        Ok(FramePoll::Complete(std::mem::take(&mut self.body)))
    }
}

/// Writes one length-prefixed frame, refusing oversize payloads.
///
/// Small-frame fast path for scripted peers and tests (blocking;
/// callers speak to draining readers). SIGPIPE masked per write.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` when the payload exceeds
/// `max_frame` or the write fails (EPIPE on a dead peer arrives as
/// a typed error, never a signal).
pub fn write_frame(writer: &mut impl Write, payload: &[u8], max_frame: usize) -> Result<()> {
    if payload.len() > max_frame {
        return Err(protocol_error(format!(
            "frame length {} exceeds maximum {max_frame}",
            payload.len()
        )));
    }
    let header = (payload.len() as u32).to_be_bytes();
    match masked(|| {
        writer
            .write_all(&header)
            .and_then(|()| writer.write_all(payload))
            .and_then(|()| writer.flush())
    }) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(protocol_error(format!("frame write: {e}"))),
        Err(e) => Err(e),
    }
}

/// `isolator.await_result` op name: the only exchange permitted an
/// unbounded (cancellable, never timed) wait. The harness lifetime
/// is uncapped by design; every other op carries a finite budget.
pub const AWAIT_RESULT_OP: &str = "isolator.await_result";

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

/// Frame exchange over a byte stream: correlation, negotiation, bounds.
///
/// No process management: timeouts bound every read, request IDs are
/// per-connection unique by construction, and responses must echo the
/// live request ID (unknown or duplicate IDs refuse). After a
/// streaming terminal frame the host stops reading; a stray
/// post-terminal frame surfaces as an ID mismatch on the next
/// exchange.
pub struct Exchange<R: Read + AsFd, W: Write + AsFd> {
    reader: R,
    writer: Option<W>,
    max_frame: usize,
    next_id: u64,
    live_ids: HashSet<String>,
}

impl<R: Read + AsFd, W: Write + AsFd> Exchange<R, W> {
    /// Opens an exchange with the pre-negotiation ceiling in force.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer: Some(writer),
            max_frame: PRE_NEGOTIATION_MAX_FRAME,
            next_id: 0,
            live_ids: HashSet::new(),
        }
    }

    /// Current enforced maximum frame.
    #[must_use]
    pub fn max_frame(&self) -> usize {
        self.max_frame
    }

    /// Closes the write side (guest sees EOF); sends refuse after.
    pub fn close(&mut self) {
        self.writer = None;
    }

    /// Drains the read side to EOF on a bounded budget, discarding bytes.
    ///
    /// Supervision hook: after the kill, EOF proves no descendant
    /// retains the read pipe; a blocked EOF reports holder residue.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` when EOF does not arrive in
    /// budget or the drain fails.
    pub(crate) fn drain_reader_to_eof(&mut self, deadline: Instant) -> Result<()> {
        let mut chunk = [0u8; 8192];
        loop {
            if !wait_readable(&self.reader, deadline)? {
                return Err(protocol_error(
                    "residue: descendant retains protocol FDs (no stdout EOF)",
                ));
            }
            match self.reader.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(protocol_error(format!("stdout EOF drain: {e}"))),
            }
        }
    }

    /// Mints the next per-connection unique request ID.
    ///
    /// Host-minted integers (`req-0`, `req-1`, ...); the string form
    /// is wire-stable for future counter widening.
    fn mint_id(&mut self) -> String {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        id
    }

    /// Sends one envelope frame bounded by `deadline`.
    ///
    /// A full stdin pipe consumes budget, never an unbounded block.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on a closed exchange,
    /// oversize, budget exhaustion, or write failure.
    pub fn send(&mut self, envelope: &Envelope, deadline: Instant) -> Result<()> {
        let max_frame = self.max_frame;
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| protocol_error("exchange is closed"))?;
        write_frame_deadline(writer, &envelope_bytes(envelope), max_frame, deadline)
    }

    /// Receives one envelope frame and parses it.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on framing, timeout, or
    /// envelope errors.
    pub fn recv(&mut self, timeout: Duration) -> Result<Envelope> {
        let body = read_frame(&mut self.reader, self.max_frame, timeout)?;
        parse_envelope(&body)
    }

    /// Raw stream reader for dispatcher-style demultiplexing.
    ///
    /// Advanced use only: callers that bypass [`Exchange::recv`]
    /// take over framing (e.g. [`StreamReader`] across poll
    /// slices) and correlation (request ids) themselves. The
    /// envelope send path stays on the exchange.
    pub fn reader_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Runs the hello exchange and negotiates the session.
    ///
    /// Sends the host hello, reads one frame under the
    /// pre-negotiation ceiling, and refuses version mismatch
    /// fail-closed before any planning. A successful negotiation
    /// installs the enforced maximum (`min(guest, host ceiling)`).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on framing errors, version
    /// mismatch, or a non-hello response.
    pub fn hello(&mut self, capabilities: &[String], timeout: Duration) -> Result<Negotiated> {
        let request = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: "hello".to_string(),
            op: "hello".to_string(),
            payload: serde_json::to_value(HelloRequest {
                version: PROTOCOL_MAJOR,
                capabilities: capabilities.to_vec(),
            })
            .expect("hello request serializes"),
        };
        // One end-to-end budget for send + receive.
        let deadline = Instant::now() + timeout;
        self.send(&request, deadline)?;
        let response = self.recv(deadline.saturating_duration_since(Instant::now()))?;
        if response.protocol != PROTOCOL_MAJOR || response.id != "hello" {
            return Err(protocol_error("hello correlation failed"));
        }
        if response.op != "hello" {
            return Err(protocol_error("expected hello response"));
        }
        let hello: HelloResponse = serde_json::from_value(response.payload)
            .map_err(|_| protocol_error("bad hello payload: shape violation"))?;
        if hello.version != PROTOCOL_MAJOR {
            return Err(protocol_error(format!(
                "unsupported guest version {}",
                hello.version
            )));
        }
        let max_frame = hello
            .max_frame
            .map(|n| (n as usize).min(HOST_MAX_FRAME))
            .unwrap_or(DEFAULT_MAX_FRAME);
        self.max_frame = max_frame;
        Ok(Negotiated {
            capabilities: hello.capabilities,
            max_frame,
        })
    }

    /// Sends one request and collects the single terminal response.
    ///
    /// A `{pending: true}` frame on a single-shot exchange refuses:
    /// streaming belongs to [`Exchange::request_stream`]. One
    /// end-to-end budget covers send + receive.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on correlation failure,
    /// pending-on-solo, timeout, or transport errors.
    pub fn request(&mut self, op: &str, payload: Value, timeout: Duration) -> Result<Value> {
        let id = self.mint_id();
        self.live_ids.insert(id.clone());
        let deadline = Instant::now() + timeout;
        let result = self.request_inner(&id, op, payload, deadline);
        self.live_ids.remove(&id);
        result
    }

    /// Sends one request and collects pending frames plus the terminal.
    ///
    /// Zero or more `{pending: true}` frames precede exactly one
    /// terminal frame under the same echoed ID; cancellation returns
    /// an error and leaves the guest alive — the caller drives guest
    /// shutdown (in-process parity kills the group; wire peers drop
    /// the connection to detach).
    ///
    /// `timeout` is `Some` budget for every op EXCEPT
    /// [`AWAIT_RESULT_OP`], whose harness lifetime is uncapped by
    /// design: `None` waits until terminal or cancellation, and any
    /// other op with `None` refuses (guests never choose unbounded
    /// waits; only the framework-selected await runs open-ended).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on correlation failure,
    /// cancellation, timeout, unbounded misuse, or transport errors.
    pub fn request_stream(
        &mut self,
        op: &str,
        payload: Value,
        timeout: Option<Duration>,
        cancel: &CancelFlag,
    ) -> Result<StreamOutcome> {
        if timeout.is_none() && op != AWAIT_RESULT_OP {
            return Err(protocol_error(format!(
                "unbounded wait reserved for {AWAIT_RESULT_OP}: {op}"
            )));
        }
        let id = self.mint_id();
        self.live_ids.insert(id.clone());
        let deadline = timeout.map(|budget| Instant::now() + budget);
        let envelope = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: id.clone(),
            op: op.to_string(),
            payload,
        };
        // Unbounded streams still bound the single send.
        let send_deadline = deadline.unwrap_or_else(|| Instant::now() + SEND_BUDGET);
        self.send(&envelope, send_deadline)?;
        if deadline.is_none() {
            return self.collect_unbounded(&id, cancel);
        }
        let mut pending = 0u32;
        loop {
            if cancel.is_cancelled() {
                self.live_ids.remove(&id);
                return Err(protocol_error("stream cancelled"));
            }
            let budget = match deadline {
                Some(end) => {
                    let now = Instant::now();
                    if now >= end {
                        self.live_ids.remove(&id);
                        return Err(protocol_error(format!("{op} timed out")));
                    }
                    end - now
                }
                // Unreachable: None returns above, but the match must
                // stay total over the option.
                None => READ_POLL_BUDGET,
            };
            let response = self.recv(budget)?;
            Self::check_correlation(&response, &id)?;
            if is_pending(&response.payload) {
                pending += 1;
                continue;
            }
            self.live_ids.remove(&id);
            return Ok(StreamOutcome {
                pending,
                result: response.payload,
            });
        }
    }

    /// Collects an unbounded `await_result` stream: idle slices poll
    /// again (cancellation stays live), mid-frame progress keeps its
    /// assembler state across slices (parsing never restarts), and
    /// only terminal/cancel/error ends the wait.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on correlation failure,
    /// cancellation, framing errors, or transport errors — never on
    /// idle silence, however long.
    fn collect_unbounded(&mut self, id: &str, cancel: &CancelFlag) -> Result<StreamOutcome> {
        let mut assembler = FrameAssembler::new();
        let mut pending = 0u32;
        loop {
            if cancel.is_cancelled() {
                self.live_ids.remove(id);
                return Err(protocol_error("stream cancelled"));
            }
            let slice = Instant::now() + READ_POLL_BUDGET;
            match assembler.poll_once(&mut self.reader, self.max_frame, slice)? {
                FramePoll::Complete(body) => {
                    let response = parse_envelope(&body)?;
                    Self::check_correlation(&response, id)?;
                    if is_pending(&response.payload) {
                        pending += 1;
                        continue;
                    }
                    self.live_ids.remove(id);
                    return Ok(StreamOutcome {
                        pending,
                        result: response.payload,
                    });
                }
                // Idle silence and mid-frame progress both poll again;
                // the assembler keeps partial bytes across slices.
                FramePoll::Idle | FramePoll::Partial => continue,
            }
        }
    }

    fn request_inner(
        &mut self,
        id: &str,
        op: &str,
        payload: Value,
        deadline: Instant,
    ) -> Result<Value> {
        let envelope = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: id.to_string(),
            op: op.to_string(),
            payload,
        };
        self.send(&envelope, deadline)?;
        let response = self.recv(deadline.saturating_duration_since(Instant::now()))?;
        Self::check_correlation(&response, id)?;
        if is_pending(&response.payload) {
            return Err(protocol_error(format!(
                "{op} sent pending on a single-shot exchange"
            )));
        }
        Ok(response.payload)
    }

    fn check_correlation(response: &Envelope, id: &str) -> Result<()> {
        if response.protocol != PROTOCOL_MAJOR {
            return Err(protocol_error("response protocol mismatch"));
        }
        // IDs are grammar-checked at parse; the diagnostic names no
        // guest string (value-free).
        if response.id != id {
            return Err(protocol_error("response for unknown or duplicate id"));
        }
        Ok(())
    }
}
