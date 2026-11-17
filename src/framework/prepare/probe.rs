//! Probe and attestation report parsing: exact wire shapes plus the
//! drain-and-parse helpers.
//!
//! Split from `prepare.rs` at the file-size limit.

use crate::error::{CistellaError, Result};
use crate::framework::registry::{MIN_LANDLOCK_ABI, REQUIRED_HANDLED_FS};

/// Exact `--probe` success shape: kernel ABI plus handled mask.
/// Derived `Deserialize` rejects duplicate fields and (with
/// `deny_unknown_fields`) any extra keys — the report is an exact
/// contract, not a loose map.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeOk {
    abi: u64,
    handled_fs_mask: u64,
}

/// Exact `--probe` refusal shape: wrapper-observed unsupported reason.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeUnsupported {
    unsupported: String,
}

/// Exact probe report: success or refusal, nothing else.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ProbeReport {
    Ok(ProbeOk),
    Unsupported(ProbeUnsupported),
}

/// Exact attestation shape: applied with ABI and handled mask,
/// or refused with a reason. Derived `Deserialize` rejects
/// duplicate fields and extra keys; the cross-field check below
/// enforces the tagged pairing (abi+mask xor error). The mask
/// repeats so the gate re-verifies the matrix at this second
/// trust moment (probe ran earlier as a separate `podman exec`
/// on stdout, not the same pipe, so no same-pipe provenance is
/// claimed). Independent absolute floors (ABI minimum, mask
/// coverage) are the contract; exact probe/apply match is not
/// required (the kernel ABI is fixed per boot, and wrapper
/// construction enforces real adequacy).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Attestation {
    applied: bool,
    abi: Option<u64>,
    handled_fs_mask: Option<u64>,
    error: Option<String>,
}

/// Checks a wrapper `--probe` report: kernel ABI plus handled
/// mask. Shortfall refuses typed (fail pre-execute); the mask is
/// never narrowed to fit the kernel.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shape violation,
/// wrapper-reported unsupported, low ABI, or rights shortfall.
pub fn parse_probe_report(stdout: &[u8]) -> Result<()> {
    let report: ProbeReport =
        serde_json::from_slice(stdout).map_err(|_| {
            CistellaError::Contract("bad probe report: shape".to_string())
        })?;
    match report {
        ProbeReport::Unsupported(unsupported) => Err(CistellaError::Contract(
            format!("landlock unsupported: {}", unsupported.unsupported),
        )),
        ProbeReport::Ok(ok) => {
            if ok.abi < u64::from(MIN_LANDLOCK_ABI) {
                return Err(CistellaError::Contract(format!(
                    "landlock unsupported: kernel ABI {} below minimum {}",
                    ok.abi, MIN_LANDLOCK_ABI
                )));
            }
            if ok.handled_fs_mask & REQUIRED_HANDLED_FS != REQUIRED_HANDLED_FS
            {
                return Err(CistellaError::Contract(
                    "landlock unsupported: kernel rights shortfall"
                        .to_string(),
                ));
            }
            Ok(())
        },
    }
}

/// Parses one wrapper attestation line: returns `(ABI, mask)` on
/// an exact applied shape whose matrix meets the floor
/// (ABI≥minimum, mask covering the required rights). `applied:false`
/// or any other shape refuses typed — session start never proceeds
/// past a failed apply or a short matrix, even attested.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shape violation (including
/// duplicate or unknown fields), a negative attestation, a low
/// ABI, or a rights shortfall.
pub fn parse_attestation_line(line: &str) -> Result<(u64, u64)> {
    let attestation: Attestation =
        serde_json::from_str(line).map_err(|_| {
            CistellaError::Contract("bad attestation: shape".to_string())
        })?;
    match (
        attestation.applied,
        attestation.abi,
        attestation.handled_fs_mask,
        attestation.error,
    ) {
        (true, Some(abi), Some(mask), None) => {
            if abi < u64::from(MIN_LANDLOCK_ABI) {
                return Err(CistellaError::Contract(format!(
                    "applied attestation ABI {abi} below minimum {}",
                    MIN_LANDLOCK_ABI
                )));
            }
            if mask & REQUIRED_HANDLED_FS != REQUIRED_HANDLED_FS {
                return Err(CistellaError::Contract(
                    "applied attestation rights shortfall".to_string(),
                ));
            }
            Ok((abi, mask))
        },
        (false, _, _, Some(error)) => Err(CistellaError::Contract(format!(
            "wrapper reported apply failure: {error}"
        ))),
        _ => Err(CistellaError::Contract(
            "bad attestation: shape".to_string(),
        )),
    }
}

/// Reads one attestation line from a diagnostics read-end under an
/// absolute deadline: polls for readability, accumulates to the
/// first newline (64 KiB cap), and returns the line plus any
/// already-buffered trailing bytes (they belong to the drain
/// phase, never discarded). EOF before the newline refuses typed
/// (the wrapper died before attesting); timeout refuses typed.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on EOF, timeout, overlong
/// output, or wait failure, and `CistellaError::Runtime` on read
/// failure.
pub fn read_attestation_line(
    read: std::os::fd::BorrowedFd<'_>,
    timeout: std::time::Duration,
) -> Result<(String, Vec<u8>)> {
    use nix::poll::{PollFd, PollFlags, poll};
    use std::os::fd::{AsFd, AsRawFd};
    let deadline = std::time::Instant::now() + timeout;
    let mut buffered = Vec::new();
    loop {
        if let Some(position) = buffered.iter().position(|&byte| byte == b'\n')
        {
            let line =
                String::from_utf8_lossy(&buffered[..position]).into_owned();
            let rest = buffered[position + 1..].to_vec();
            return Ok((line, rest));
        }
        if buffered.len() > 64 * 1024 {
            return Err(CistellaError::Contract(
                "hook diagnostics overlong before attestation".to_string(),
            ));
        }
        let wait = nix::poll::PollTimeout::try_from(
            deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(std::time::Duration::ZERO),
        )
        .map_err(|_| {
            CistellaError::Runtime("attestation wait out of range".to_string())
        })?;
        let mut pollfds = [PollFd::new(read.as_fd(), PollFlags::POLLIN)];
        let ready = poll(&mut pollfds, wait).map_err(|e| {
            CistellaError::Contract(format!("attestation wait failed: {e}"))
        })?;
        if ready == 0 {
            return Err(CistellaError::Contract(
                "hook attestation timed out".to_string(),
            ));
        }
        let mut chunk = [0u8; 8192];
        let raw = read.as_fd().as_raw_fd();
        // SAFETY: borrowed read-end, transient buffer, return
        // checked below; no ownership transfer.
        let count = unsafe {
            nix::libc::read(raw, chunk.as_mut_ptr().cast(), chunk.len())
        };
        if count < 0 {
            return Err(CistellaError::Runtime(
                "diagnostics read failed".to_string(),
            ));
        }
        if count == 0 {
            return Err(CistellaError::Contract(
                "hook diagnostics closed before attestation".to_string(),
            ));
        }
        buffered.extend_from_slice(&chunk[..count as usize]);
    }
}
