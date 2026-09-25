//! Incremental frame reader preserving assembly across polls.
//!
//! Split from [`protocol`](super::protocol) at the supervision
//! seam (file-size limit): the assembler stays where the framing
//! rules live; only the persistent polling shell moves here.

use std::io::Read;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::framework::protocol::{FrameAssembler, FramePoll, protocol_error};

/// Incremental frame reader preserving assembly across polls.
///
/// Unlike one-shot [`read_frame`], which discards partial progress
/// on timeout, this reader keeps the assembler alive across calls:
/// a frame trickling across many poll slices still resolves, and
/// slow peers never corrupt correlation. Demultiplexing loops
/// (client dispatchers) poll once per slice and act on whatever
/// each slice yields.
pub struct StreamReader {
    /// Persistent assembler (survives across slices).
    assembler: FrameAssembler,
    /// Frame ceiling (refused before allocation, every slice).
    max_frame: usize,
}

impl StreamReader {
    /// Empty reader for the frame ceiling.
    #[must_use]
    pub fn new(max_frame: usize) -> Self {
        Self {
            assembler: FrameAssembler::new(),
            max_frame,
        }
    }

    /// Polls once toward a frame: `Ok(Some)` on completion,
    /// `Ok(None)` on an idle slice (call again), `Err` on
    /// oversize, truncation, IO, or slice-budget exhaustion.
    /// A budget-exhausted slice with zero bytes consumed is idle;
    /// with bytes consumed it is a stalled trickle (failure),
    /// mirroring [`read_frame`] without discarding state.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on oversize, truncation,
    /// IO failure, or a stalled trickle past the slice budget.
    pub fn poll_frame(
        &mut self,
        reader: &mut (impl Read + AsFd),
        budget: Duration,
    ) -> Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + budget;
        loop {
            if Instant::now() >= deadline {
                if self.assembler.is_fresh() {
                    return Ok(None);
                }
                return Err(protocol_error("frame read timed out"));
            }
            match self.assembler.poll_once(reader, self.max_frame, deadline)? {
                FramePoll::Complete(body) => return Ok(Some(body)),
                FramePoll::Idle => return Ok(None),
                FramePoll::Partial => continue,
            }
        }
    }
}
