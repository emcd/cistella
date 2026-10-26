//! Length-prefixed frame codec and the log reader.
//!
//! Encoding, decoding with the declared corruption handling, and
//! reading one operation directory back into records plus a
//! derived outcome. Readers never rewrite the raw log.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::{MAX_FRAME_PAYLOAD, PER_OP_CAP, RECORDS_NAME};
use crate::error::{CistellaError, Result};

/// Outcome of one operation as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Terminal record present with this outcome string.
    Completed(String),
    /// No terminal record, or the file is torn before one:
    /// unknown, never success and never proof of inaction.
    Unknown,
}

/// Reader result for one operation directory.
#[derive(Debug, Clone)]
pub struct ReadResult {
    /// Complete valid records in sequence order.
    pub records: Vec<serde_json::Value>,
    /// Frames discarded (torn prefix, short payload, invalid
    /// JSON/schema, sequence gap), counted for the derived result.
    pub discards: u64,
    /// Operation outcome as read.
    pub outcome: Outcome,
}

/// Reads one control file with three guarantees: the open never
/// blocks on a FIFO (O_NONBLOCK), a symlink or any other
/// non-regular file is refused (O_NOFOLLOW plus an fstat type
/// check on the opened description, so replacement after open
/// cannot fool it), and the read never allocates past `ceiling`
/// no matter what the file claims or grows into. Hitting the
/// ceiling refuses the read; it is never a truncation. Used by
/// every production control-file reader (log reads, prune
/// candidate reads) so no path blocks on or over-allocates from
/// a planted or replaced file.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the file cannot be
/// opened, is not a regular file, cannot be read, or exceeds the
/// ceiling.
pub(crate) fn read_bounded_regular(path: &Path, ceiling: u64) -> Result<Vec<u8>> {
    use std::os::fd::FromRawFd;
    let raw = nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NONBLOCK
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|errno| CistellaError::Runtime(format!("operation log read: {errno}")))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let file: File = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw).into() };
    // fstat on the open description: a symlink target aside, a
    // FIFO or directory passes nofollow but fails here, and a
    // regular file swapped in after this check was already
    // opened (the fd pins the file), so the read below is safe.
    if !file
        .metadata()
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
    {
        return Err(CistellaError::Runtime(
            "operation log read is not a regular file".to_string(),
        ));
    }
    let mut bytes = Vec::new();
    // O_NONBLOCK is harmless on the regular files this admits:
    // their reads never block. The bound is enforced by the read
    // itself, not by any size pre-check.
    (&file)
        .take(ceiling + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| CistellaError::Runtime(format!("operation log read: {error}")))?;
    if bytes.len() as u64 > ceiling {
        return Err(CistellaError::Runtime(
            "operation log read exceeds bound".to_string(),
        ));
    }
    Ok(bytes)
}

/// Encodes one frame: 4-byte big-endian length plus JSON bytes.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the payload exceeds the
/// maximum frame payload.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(CistellaError::Runtime(format!(
            "operation log frame {} exceeds {MAX_FRAME_PAYLOAD}",
            payload.len()
        )));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Decodes frames from a byte buffer per the reader contract:
/// length checked against the maximum payload and the remaining
/// bytes before any allocation; partial prefix, short payload,
/// invalid JSON, or sequence gap discards that frame and everything
/// after it while preserving the complete valid prefix.
#[must_use]
pub fn decode_frames(bytes: &[u8]) -> (Vec<serde_json::Value>, u64) {
    let mut records = Vec::new();
    let mut discards = 0u64;
    let mut rest = bytes;
    let mut expected_seq = 0u64;
    let mut seen_start = false;
    loop {
        if rest.is_empty() {
            break;
        }
        // A partial length prefix cannot name a frame: discard.
        if rest.len() < 4 {
            discards += 1;
            break;
        }
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        // The declared length is checked against the maximum
        // payload AND the remaining file bytes BEFORE any
        // allocation: an oversize claim or a short payload ends
        // the valid prefix here.
        if len > MAX_FRAME_PAYLOAD || len > rest.len() - 4 {
            discards += 1;
            break;
        }
        let payload = &rest[4..4 + len];
        let value: serde_json::Value = match serde_json::from_slice(payload) {
            Ok(value) => value,
            Err(_) => {
                discards += 1;
                break;
            }
        };
        // Declared schema plus state machine, in order: a numeric
        // `seq` matching the expected position, then a known `kind`
        // valid in its position (start first and only first, body
        // while open, terminal to close). Anything else ends the
        // valid prefix here; the terminal must end the stream, so
        // trailing bytes after one are corruption, not records.
        let seq_ok = value.get("seq").and_then(serde_json::Value::as_u64) == Some(expected_seq);
        let kind = value.get("kind").and_then(serde_json::Value::as_str);
        let shape_ok = matches!(
            (seen_start, kind),
            (false, Some("start")) | (true, Some("body")) | (true, Some("terminal"))
        );
        if !seq_ok || !shape_ok {
            discards += 1;
            break;
        }
        let terminal = kind == Some("terminal");
        seen_start = true;
        records.push(value);
        expected_seq += 1;
        rest = &rest[4 + len..];
        if terminal {
            if !rest.is_empty() {
                discards += 1;
            }
            break;
        }
    }
    (records, discards)
}
/// Reads one operation directory per the reader contract. The
/// terminal outcome is accepted only with a contiguous valid
/// sequence ending in a terminal record; anything else reads
/// UNKNOWN. Discards are counted for the derived result; the raw
/// file is never rewritten.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the records file cannot
/// be opened or is larger than the read bound.
pub fn read_operation(dir: &Path) -> Result<ReadResult> {
    let path = dir.join(RECORDS_NAME);
    // Bounded read: the byte ceiling is enforced by the read
    // itself, not by a metadata pre-check, so a file that grows or
    // is replaced between stat and read cannot over-allocate.
    // Hitting the ceiling means the file is not a well-formed
    // operation log and it is refused, not truncated.
    let ceiling = PER_OP_CAP + MAX_FRAME_PAYLOAD as u64 + 4096;
    let bytes = read_bounded_regular(&path, ceiling)?;
    let (records, discards) = decode_frames(&bytes);
    let outcome = read_outcome(&records);
    Ok(ReadResult {
        records,
        discards,
        outcome,
    })
}

/// Derives the outcome: terminal record with a contiguous valid
/// sequence reads completed; anything else reads UNKNOWN.
pub(crate) fn read_outcome(records: &[serde_json::Value]) -> Outcome {
    let mut kinds: Vec<&str> = Vec::new();
    for record in records {
        match record.get("kind").and_then(serde_json::Value::as_str) {
            Some(kind @ ("start" | "body" | "terminal")) => kinds.push(kind),
            _ => return Outcome::Unknown,
        }
    }
    if kinds.first() != Some(&"start") {
        return Outcome::Unknown;
    }
    if kinds.last() != Some(&"terminal") {
        return Outcome::Unknown;
    }
    match records
        .last()
        .and_then(|record| record.get("outcome"))
        .and_then(serde_json::Value::as_str)
    {
        Some(outcome) => Outcome::Completed(outcome.to_string()),
        None => Outcome::Unknown,
    }
}
