//! Framework-owned local digest registry for hook artifacts (task 3.1).
//!
//! Minimal by Advisory ruling: one admitted registry (`shipped`)
//! rooted at the install sibling directory, serving the one shipped
//! wrapper. No remote fetch, no generalized artifact service.
//!
//! Resolution pins and hashes the registry bytes and compares
//! against the extension-advertised digest; mismatch refuses. The
//! digest binds the resolved bytes to the request — the trust anchor
//! stays the operator-owned install directory (same-crate trust
//! assumption), never the extension's word. Staging (verified copy,
//! RO guest mount) rides task 3.2; this module resolves only.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{CistellaError, Result};

/// Admitted registry id: the installed sibling directory.
pub const SHIPPED_REGISTRY_ID: &str = "shipped";

/// Wrapper file name inside the shipped registry.
pub const WRAPPER_FILE_NAME: &str = "cistella-landlock-wrap";

/// Known guest path where the isolator stages the wrapper RO
/// pre-initiate; the hook `argv_prefix` names exactly this path.
pub const STAGED_WRAPPER_GUEST_PATH: &str = "/run/cistella/hooks/landlock-wrap";

/// Canonical guest-context probe operation for the Landlock hook.
pub const LANDLOCK_PROBE_OP: &str = "probe_capabilities";

/// Refusal ceiling for registry reads: a wrapper binary is small;
/// an oversized registry file refuses rather than buffering
/// unboundedly.
const MAX_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;

/// Digest-verified registry bytes plus their coordinates.
#[derive(Debug, Clone)]
pub struct PinnedArtifact {
    /// Verified file bytes (digest matches the request).
    pub bytes: Vec<u8>,
    /// SHA-256 over `bytes`.
    pub sha256: [u8; 32],
    /// Admitted registry id.
    pub registry: String,
    /// Registry-relative path.
    pub path: String,
}

/// Resolves and digest-verifies one hook artifact reference.
///
/// `exe_dir` is the install sibling directory (never PATH).
/// Admission: only [`SHIPPED_REGISTRY_ID`]. The path must be
/// relative with normal segments only (no escapes, no absolute);
/// the digest must match the pinned bytes exactly.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on unknown registry, bad
/// path shape, digest mismatch, or oversized file, and
/// `CistellaError::Io` on read failure.
pub fn resolve(
    exe_dir: &Path,
    registry: &str,
    path: &str,
    expected_sha256: &str,
) -> Result<PinnedArtifact> {
    if registry != SHIPPED_REGISTRY_ID {
        return Err(CistellaError::Contract(format!(
            "unknown artifact registry: {registry}"
        )));
    }
    let relative = check_relative_path(path)?;
    let full = exe_dir.join(&relative);
    let expected = parse_sha256(expected_sha256)?;
    let metadata = std::fs::metadata(&full)?;
    if metadata.len() > MAX_REGISTRY_BYTES {
        return Err(CistellaError::Contract(format!(
            "artifact exceeds registry read ceiling: {path}"
        )));
    }
    let bytes = std::fs::read(&full)?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if digest != expected {
        return Err(CistellaError::Contract(format!(
            "artifact digest mismatch: {path}"
        )));
    }
    Ok(PinnedArtifact {
        bytes,
        sha256: digest,
        registry: registry.to_string(),
        path: path.to_string(),
    })
}

/// Reads one registry file and returns its lowercase hex SHA-256.
///
/// Sibling-binary reuse: the extension guest advertises the wrapper
/// digest it observes without reimplementing hashing. Missing or
/// unreadable files surface as `Io`; the caller fails closed.
///
/// # Errors
///
/// Returns `CistellaError::Io` on read failure.
pub fn digest_sibling(exe_dir: &Path, file_name: &str) -> Result<String> {
    let bytes = std::fs::read(exe_dir.join(file_name))?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok(hex)
}
/// Rejects empty, absolute, and escaping registry paths; returns
/// the relative join path. Diagnostics name the refusal class, not
/// the offending bytes beyond the path itself (registry paths are
/// framework-visible coordinates, not harness secrets).
fn check_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        return Err(CistellaError::Contract(
            "artifact path must not be empty".to_string(),
        ));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(segment) => relative.push(segment),
            _ => {
                return Err(CistellaError::Contract(format!(
                    "artifact path must be registry-relative without escapes: {path}"
                )));
            }
        }
    }
    Ok(relative)
}

/// Parses 64 lowercase hex into raw digest bytes (same shape the
/// merge gate enforces, so a merged hook always parses here).
fn parse_sha256(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CistellaError::Contract(
            "artifact sha256 must be 64 lowercase hex".to_string(),
        ));
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk)
            .map_err(|_| CistellaError::Contract("artifact sha256 must be hex".to_string()))?;
        out[index] = u8::from_str_radix(text, 16)
            .map_err(|_| CistellaError::Contract("artifact sha256 must be hex".to_string()))?;
    }
    Ok(out)
}
