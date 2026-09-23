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
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nix::sys::select::{FdSet, select};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, kill, sigaction};
use nix::sys::time::TimeVal;
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CistellaError, Result};
use crate::framework::contract::CancelFlag;

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

/// Stderr drain accounting (content discarded, counts only).
#[derive(Debug, Clone, Copy)]
pub struct StderrDrain {
    /// Total stderr bytes drained.
    pub bytes: u64,
    /// True when output exceeded [`STDERR_CAP`].
    pub truncated: bool,
}

fn protocol_error(message: impl Into<String>) -> CistellaError {
    CistellaError::Protocol(message.into())
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
fn wait_readable(fd: &impl AsFd, deadline: Instant) -> Result<bool> {
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

/// Reads exactly `buf.len()` bytes before `deadline`.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on timeout (budget exhausted) or
/// EOF/truncation mid-frame.
fn read_exact_deadline(
    reader: &mut (impl Read + AsFd),
    mut buf: &mut [u8],
    deadline: Instant,
) -> Result<()> {
    while !buf.is_empty() {
        if !wait_readable(reader, deadline)? {
            return Err(protocol_error("frame read timed out"));
        }
        match reader.read(buf) {
            Ok(0) => return Err(protocol_error("truncated frame: EOF mid-frame")),
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(protocol_error(format!("frame read: {e}"))),
        }
    }
    Ok(())
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
    let mut header = [0u8; HEADER_LEN];
    read_exact_deadline(reader, &mut header, deadline)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > max_frame {
        return Err(protocol_error(format!(
            "frame length {len} exceeds maximum {max_frame}"
        )));
    }
    let mut body = vec![0u8; len];
    read_exact_deadline(reader, &mut body, deadline)?;
    Ok(body)
}

/// Writes one length-prefixed frame, refusing oversize payloads.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` when the payload exceeds
/// `max_frame` or the write fails (including EPIPE on a dead guest —
/// the host ignores SIGPIPE while a guest lives, so death arrives as
/// a typed error, never a signal).
pub fn write_frame(writer: &mut impl Write, payload: &[u8], max_frame: usize) -> Result<()> {
    if payload.len() > max_frame {
        return Err(protocol_error(format!(
            "frame length {} exceeds maximum {max_frame}",
            payload.len()
        )));
    }
    let header = (payload.len() as u32).to_be_bytes();
    writer
        .write_all(&header)
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(|e| protocol_error(format!("frame write: {e}")))
}

/// Parses and validates one envelope: UTF-8, JSON object, known fields.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on invalid UTF-8, malformed
/// JSON, non-object envelopes, or unknown fields.
pub fn parse_envelope(body: &[u8]) -> Result<Envelope> {
    let text = std::str::from_utf8(body).map_err(|_| protocol_error("frame is not UTF-8"))?;
    serde_json::from_str(text).map_err(|e| protocol_error(format!("bad envelope: {e}")))
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
pub struct Exchange<R: Read + AsFd, W: Write> {
    reader: R,
    writer: Option<W>,
    max_frame: usize,
    next_id: u64,
    live_ids: HashSet<String>,
}

impl<R: Read + AsFd, W: Write> Exchange<R, W> {
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

    /// Mints the next per-connection unique request ID.
    ///
    /// Host-minted integers (`req-0`, `req-1`, ...); the string form
    /// is wire-stable for future counter widening.
    fn mint_id(&mut self) -> String {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        id
    }

    /// Sends one envelope frame.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on a closed exchange,
    /// oversize, or write failure.
    pub fn send(&mut self, envelope: &Envelope) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| protocol_error("exchange is closed"))?;
        write_frame(writer, &envelope_bytes(envelope), self.max_frame)
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
        self.send(&request)?;
        let response = self.recv(timeout)?;
        if response.protocol != PROTOCOL_MAJOR || response.id != "hello" {
            return Err(protocol_error("hello correlation failed"));
        }
        if response.op != "hello" {
            return Err(protocol_error(format!(
                "expected hello response, got op {}",
                response.op
            )));
        }
        let hello: HelloResponse = serde_json::from_value(response.payload)
            .map_err(|e| protocol_error(format!("bad hello payload: {e}")))?;
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
    /// streaming belongs to [`Exchange::request_stream`].
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on correlation failure,
    /// pending-on-solo, timeout, or transport errors.
    pub fn request(&mut self, op: &str, payload: Value, timeout: Duration) -> Result<Value> {
        let id = self.mint_id();
        self.live_ids.insert(id.clone());
        let result = self.request_inner(&id, op, payload, timeout);
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
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on correlation failure,
    /// cancellation, timeout, or transport errors.
    pub fn request_stream(
        &mut self,
        op: &str,
        payload: Value,
        timeout: Duration,
        cancel: &CancelFlag,
    ) -> Result<StreamOutcome> {
        let id = self.mint_id();
        self.live_ids.insert(id.clone());
        let deadline = Instant::now() + timeout;
        let envelope = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: id.clone(),
            op: op.to_string(),
            payload,
        };
        self.send(&envelope)?;
        let mut pending = 0u32;
        loop {
            if cancel.is_cancelled() {
                self.live_ids.remove(&id);
                return Err(protocol_error("stream cancelled"));
            }
            let now = Instant::now();
            if now >= deadline {
                self.live_ids.remove(&id);
                return Err(protocol_error(format!("{op} timed out")));
            }
            let response = self.recv(deadline - now)?;
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

    fn request_inner(
        &mut self,
        id: &str,
        op: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let envelope = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: id.to_string(),
            op: op.to_string(),
            payload,
        };
        self.send(&envelope)?;
        let response = self.recv(timeout)?;
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
        if response.id != id {
            return Err(protocol_error(format!(
                "response for unknown or duplicate id: {}",
                response.id
            )));
        }
        Ok(())
    }
}

/// Guest process host: spawn, speak, kill, reap, drain.
///
/// Owns the whole guest lifetime: pinned executable (absolute path
/// required, never PATH-searched), own process group (descendants
/// that fork, change groups, retain FDs, or fill pipes die with the
/// group), concurrent bounded stderr drain (content discarded, counts
/// only), and reverse-order shutdown with residue-dominated
/// reporting. While a guest lives, SIGPIPE is ignored process-wide
/// (saved and restored at shutdown) so a dead guest arrives as a
/// typed write error, never a signal.
pub struct GuestHost<R: Read + AsFd, W: Write> {
    exchange: Exchange<R, W>,
    child: Child,
    stderr_outcome: mpsc::Receiver<StderrDrain>,
    old_sigpipe: Option<SigAction>,
    deadlines: crate::framework::contract::Deadlines,
    shut: bool,
}

impl GuestHost<ChildStdout, ChildStdin> {
    /// Spawns a pinned guest executable and opens the exchange.
    ///
    /// The executable must be an absolute path: helpers are
    /// discovered and pinned by the framework, never PATH-searched.
    /// Stderr drains on a background thread from spawn (content
    /// discarded immediately); the child leads its own process group.
    ///
    /// Exactly one guest at a time: a second `spawn` would save the
    /// first spawn's SIGPIPE-ignore as its restore target, so
    /// concurrent guests need a per-guest disposition layer (deferred).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on a relative executable or
    /// spawn failure.
    pub fn spawn(
        executable: &Path,
        args: &[String],
        deadlines: crate::framework::contract::Deadlines,
    ) -> Result<Self> {
        if !executable.is_absolute() {
            return Err(protocol_error(format!(
                "guest executable must be absolute, never PATH-searched: {}",
                executable.display()
            )));
        }
        let mut child = unsafe {
            Command::new(executable)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .pre_exec(|| {
                    nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0))
                        .map_err(std::io::Error::other)
                })
                .spawn()
        }
        .map_err(|e| protocol_error(format!("guest spawn: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            sender.send(drain_stderr(stderr)).expect("drain report");
        });
        let old_sigpipe = ignore_sigpipe();
        Ok(Self {
            exchange: Exchange::new(stdout, stdin),
            child,
            stderr_outcome: receiver,
            old_sigpipe,
            deadlines,
            shut: false,
        })
    }

    /// Child pid (observability for tests and conformance).
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Mutable frame exchange (hello, requests, streams).
    pub fn exchange_mut(&mut self) -> &mut Exchange<ChildStdout, ChildStdin> {
        &mut self.exchange
    }

    /// Kills the process group (SIGTERM, grace, SIGKILL) and reaps.
    ///
    /// Alive check first: an already-reaped guest needs no signal.
    /// Descendants fall with the group even when they retain FDs.
    fn kill_group(&mut self) -> Result<()> {
        if self
            .child
            .try_wait()
            .map_err(|e| protocol_error(format!("wait: {e}")))?
            .is_some()
        {
            return Ok(());
        }
        let group = Pid::from_raw(-(self.child.id() as i32));
        let _ = kill(group, Signal::SIGTERM);
        let grace = Instant::now() + self.deadlines.terminate_grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {}
                Err(e) => return Err(protocol_error(format!("reap: {e}"))),
            }
            if Instant::now() >= grace {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = kill(group, Signal::SIGKILL);
        self.child
            .wait()
            .map_err(|e| protocol_error(format!("reap after kill: {e}")))?;
        Ok(())
    }

    /// Reverse-order shutdown: close stdin, kill group, reap, join
    /// the stderr drain, restore SIGPIPE. Residue dominates: a
    /// kill/reap failure outranks drain or restore failures.
    ///
    /// # Errors
    ///
    /// Returns the residue-class failure when cleanup leaves residue;
    /// drain/restore failures surface only with a clean kill.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.shut {
            return Ok(());
        }
        self.shut = true;
        // Reverse acquisition: stdin pipe, process group, reap, drain.
        self.exchange.close();
        let mut residue: Option<CistellaError> = None;
        if let Err(e) = self.kill_group() {
            residue = Some(e);
        }
        let drain = self.stderr_outcome.recv_timeout(Duration::from_secs(5));
        if residue.is_none()
            && let Err(e) = drain
                .map(|_| ())
                .map_err(|_| protocol_error("stderr drain hung"))
        {
            residue = Some(e);
        }
        if let Some(old) = self.old_sigpipe.take() {
            unsafe {
                let _ = sigaction(Signal::SIGPIPE, &old);
            }
        }
        if let Some(error) = residue {
            return Err(error);
        }
        Ok(())
    }
}

/// Reads stderr to EOF, discarding content, counting bytes.
fn drain_stderr(mut stderr: ChildStderr) -> StderrDrain {
    let mut bytes = 0u64;
    let mut chunk = [0u8; 8192];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes += n as u64,
            Err(_) => break,
        }
    }
    StderrDrain {
        bytes,
        truncated: bytes > STDERR_CAP,
    }
}

/// Ignores SIGPIPE process-wide, returning the previous disposition.
fn ignore_sigpipe() -> Option<SigAction> {
    unsafe {
        let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        sigaction(Signal::SIGPIPE, &ignore).ok()
    }
}
