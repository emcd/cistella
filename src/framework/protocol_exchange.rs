use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::Result;
use crate::framework::contract::CancelFlag;

use super::protocol_frames::{AWAIT_RESULT_OP, FrameAssembler, FramePoll, read_frame};
use super::{
    DEFAULT_MAX_FRAME, Envelope, HOST_MAX_FRAME, HelloRequest, HelloResponse, Negotiated,
    PRE_NEGOTIATION_MAX_FRAME, PROTOCOL_MAJOR, READ_POLL_BUDGET, SEND_BUDGET, StreamOutcome,
    envelope_bytes, is_pending, parse_envelope, protocol_error, wait_readable,
    write_frame_deadline,
};

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
        if response.id != "hello" {
            return Err(protocol_error("hello correlation failed"));
        }
        if response.protocol != PROTOCOL_MAJOR {
            // Epoch break: name the offered version so a legacy
            // peer meets a version error, never an opaque
            // correlation failure. Payload may be undecodable;
            // the envelope protocol is authoritative here.
            return Err(protocol_error(format!(
                "unsupported guest version: envelope protocol {}",
                response.protocol
            )));
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
