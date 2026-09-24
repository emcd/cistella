//! Identity: per-seat SSH agent mount RO, allowed_signers.

use crate::error::{CistellaError, Result};
use crate::profile::{CredentialSurface, Profile};

/// Returns volume args for the per-seat `AF_UNIX` `SSH_AUTH_SOCK` mount read-only.
///
/// The socket comes ONLY from the profile's `credential_surface` slot;
/// ambient `SSH_AUTH_SOCK` is never consulted. `credential-surface = "none"`
/// mounts nothing; `credential-surface = { ssh_agent = "/run/..." }` mounts
/// that per-seat socket RO and sets `SSH_AUTH_SOCK` inside. Sign-only is a
/// GitHub key registration property (type `Signing`) plus absence of
/// auth-registered credentials; the socket itself is a normal agent.
#[must_use]
pub fn ssh_agent_volume_args(profile: &Profile) -> Vec<String> {
    match &profile.credential_surface {
        CredentialSurface::None => vec![],
        CredentialSurface::Agent { ssh_agent } => vec![
            "--volume".to_string(),
            format!("{ssh_agent}:{ssh_agent}:ro"),
            "-e".to_string(),
            format!("SSH_AUTH_SOCK={ssh_agent}"),
        ],
    }
}

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
