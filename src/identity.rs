//! Identity: per-seat signing helpers.
//!
//! Agent-socket forwarding is a declared-extension concern now
//! (the shipped ssh guest, fed by seat ambient): no bespoke
//! volume assembly lives here.

use crate::error::{CistellaError, Result};

/// Writes an `allowed_signers` file for the seat's signing key.
///
/// Format: `principal ssh-ed25519 AAAAC3...`
pub fn write_allowed_signers(path: &std::path::Path, principal: &str, pubkey: &str) -> Result<()> {
    let content = format!("{principal} {pubkey}\n");
    std::fs::write(path, content).map_err(|e| {
        CistellaError::Identity(format!("write allowed_signers {}: {e}", path.display()))
    })?;
    Ok(())
}
