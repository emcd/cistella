use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use crate::error::Result;

use super::{EOF_AT_BOUNDARY, HEADER_LEN, IDLE_TIMEOUT, masked, protocol_error, wait_readable};

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
