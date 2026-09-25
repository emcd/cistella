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
///
/// A started-but-stalled frame cannot pend forever: once any byte
/// arrives, completion is due by the frame timeout (uncapped
/// awaits bound only the harness lifetime, never control-plane
/// framing), and expiry fails typed.
pub struct StreamReader {
    /// Persistent assembler (survives across slices).
    assembler: FrameAssembler,
    /// Frame ceiling (refused before allocation, every slice).
    max_frame: usize,
    /// Completion budget once progress starts.
    frame_timeout: Duration,
    /// First-progress instant (cleared on completion).
    first_progress: Option<Instant>,
}

impl StreamReader {
    /// Empty reader for the frame ceiling and completion budget.
    #[must_use]
    pub fn new(max_frame: usize, frame_timeout: Duration) -> Self {
        Self {
            assembler: FrameAssembler::new(),
            max_frame,
            frame_timeout,
            first_progress: None,
        }
    }

    /// Polls once toward a frame: `Ok(Some)` on completion,
    /// `Ok(None)` on an idle slice (call again), `Err` on
    /// oversize, truncation, or IO failure.
    ///
    /// Slice expiry is never an error here, even with partial
    /// progress: the caller retries next slice with assembly
    /// intact, and the caller's own op deadlines bound total time
    /// (a truly stalled trickle fails at the op deadline, not the
    /// slice edge).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on oversize, truncation,
    /// or IO failure.
    pub fn poll_frame(
        &mut self,
        reader: &mut (impl Read + AsFd),
        budget: Duration,
    ) -> Result<Option<Vec<u8>>> {
        // Once any byte arrives, completion is due by the frame
        // timeout: the harness lifetime stays uncapped, but a
        // started frame that stalls fails typed instead of
        // pending forever across slices.
        if !self.assembler.is_fresh() {
            if self.first_progress.is_none() {
                self.first_progress = Some(Instant::now());
            }
            if let Some(started) = self.first_progress
                && started.elapsed() >= self.frame_timeout
            {
                return Err(protocol_error("frame stalled after first byte"));
            }
        }
        let deadline = Instant::now() + budget;
        if Instant::now() >= deadline {
            return Ok(None);
        }
        match self.assembler.poll_once(reader, self.max_frame, deadline)? {
            FramePoll::Complete(body) => {
                self.first_progress = None;
                Ok(Some(body))
            }
            // Idle and mid-slice progress both yield the slice:
            // Partial resumes next call with state intact instead
            // of failing the trickle.
            FramePoll::Idle | FramePoll::Partial => Ok(None),
        }
    }
}
