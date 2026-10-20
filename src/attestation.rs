//! Owner-written launch attestation: the commit event made durable.
//!
//! The conduct owner publishes one file per session after
//! `execute_launch` returns Ok, binding the attempt (unit,
//! reconciliation key, argv, execution handle) to the backend
//! incarnation (container ID + StartedAt) captured pre-launch and
//! re-verified post-launch. Cross-process readers (`describe
//! state`, `enter`) treat the file as historical commitment proof:
//! present + incarnation-equal means the launch committed for
//! this incarnation; anything else fails closed.
//!
//! Storage follows the established host-private state discipline:
//! files live in the uid-scoped state dir
//! (`PrepareLease::state_dir`, 0700 seat-owned via
//! `open_state_dir`), written O_EXCL 0600 and read through the
//! anchored same-FD discipline — the exact pattern cleanup state
//! already uses. The spec filename (`cistella-attestation-<id>`)
//! is preserved; the directory refines the draft's bare
//! `runtime_base` to the state dir, which is the discipline
//! Tier-2 accepted. Guests never receive the state dir as a
//! mount source by default; an explicit operator mount is an
//! operator act outside this boundary (same class as
//! cleanup-state files).

use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CistellaError, Result};

/// Read ceiling for one attestation file: JSON text with bounded
/// fields (argv is harness-chosen but conduct-validated; the
/// ceiling fails loud rather than truncating).
const ATTESTATION_CEILING: u64 = 64 * 1024;

/// Backend incarnation identity: the immutable per-creation
/// evidence. Recreate changes the ID; restart changes StartedAt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Incarnation {
    /// Full container ID at commit.
    pub container_id: String,
    /// Container StartedAt at commit (opaque backend string,
    /// compared exactly, never parsed).
    pub started_at: String,
}

/// One committed launch: attempt binding plus incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Attestation {
    /// Minted session id (matches filename + registry record).
    pub session_id: String,
    /// Container name (`cistella-<id>`).
    pub container_name: String,
    /// Launching attempt's reconciliation key (write-time
    /// binding, read-time provenance).
    pub attempt_key: String,
    /// Harness argv of the launching attempt.
    pub argv: Vec<String>,
    /// Framework execution handle returned by the launch.
    pub execution_handle: String,
    /// Backend incarnation bound at commit.
    pub incarnation: Incarnation,
}

/// Attestation file name for one session id (single-component,
/// safe for dirfd-relative operations; mirrors the cleanup-state
/// naming rule so session-id-shaped input is the only input).
fn attestation_name(session_id: &str) -> std::result::Result<String, String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(format!(
            "attestation needs a framework-minted session id: {session_id}"
        ));
    }
    Ok(format!("cistella-attestation-{session_id}"))
}

/// Opens the uid-scoped state dir under `base` (created 0700
/// seat-owned on first use; shared with cleanup state and
/// prepare leases).
fn open_attestation_dir(
    base: &Path,
) -> std::result::Result<crate::guest_anchor::AnchoredDir, String> {
    crate::guest_anchor::open_state_dir(&crate::lock::PrepareLease::state_dir(base))
}

/// Publishes one attestation exactly once (O_EXCL: a second
/// publish for the same session refuses rather than
/// overwriting — session ids are minted unique, so a clash is
/// a programmer error, never a retry).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on bad session id, state-dir
/// refusal, name clash, or I/O failure.
pub fn publish(base: &Path, attestation: &Attestation) -> Result<()> {
    let name = attestation_name(&attestation.session_id)
        .map_err(|error| CistellaError::Runtime(format!("attestation: {error}")))?;
    if attestation.session_id.is_empty()
        || attestation.container_name.is_empty()
        || attestation.attempt_key.is_empty()
        || attestation.execution_handle.is_empty()
        || attestation.incarnation.container_id.is_empty()
        || attestation.incarnation.started_at.is_empty()
    {
        return Err(CistellaError::Runtime(
            "attestation refuses empty identity fields".to_string(),
        ));
    }
    let bytes = serde_json::to_vec(attestation)
        .map_err(|e| CistellaError::Runtime(format!("encode attestation: {e}")))?;
    let dir = open_attestation_dir(base)
        .map_err(|error| CistellaError::Runtime(format!("attestation state: {error}")))?;
    let mut staged = dir
        .create_file(&name)
        .map_err(|e| CistellaError::Runtime(format!("publish attestation {name}: {e}")))?;
    staged
        .write_all(&bytes)
        .map_err(|e| CistellaError::Runtime(format!("write attestation {name}: {e}")))?;
    Ok(())
}

/// Reads one attestation: `None` when absent (normal precommit —
/// not an error), `Err` on corrupt/unreadable content with a
/// present file (fail loud, never silent demotion).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on state-dir refusal or
/// corrupt content, `CistellaError::Io` on I/O failure.
pub fn read(base: &Path, session_id: &str) -> Result<Option<Attestation>> {
    let name = attestation_name(session_id)
        .map_err(|error| CistellaError::Runtime(format!("attestation: {error}")))?;
    let dir = open_attestation_dir(base)
        .map_err(|error| CistellaError::Runtime(format!("attestation state: {error}")))?;
    let bytes = dir
        .read_file(&name, ATTESTATION_CEILING)
        .map_err(|e| CistellaError::Runtime(format!("read attestation {name}: {e}")))?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let attestation: Attestation = serde_json::from_slice(&bytes)
        .map_err(|e| CistellaError::Runtime(format!("corrupt attestation {name}: {e}")))?;
    if attestation.session_id != session_id {
        return Err(CistellaError::Runtime(format!(
            "attestation {name} names session {}",
            attestation.session_id
        )));
    }
    Ok(Some(attestation))
}

/// Reaps one attestation best-effort (teardown/gc path: residue
/// removal is litter-class, never a teardown failure).
pub fn reap(base: &Path, session_id: &str) {
    let Ok(name) = attestation_name(session_id) else {
        return;
    };
    let Ok(dir) = open_attestation_dir(base) else {
        return;
    };
    dir.remove_file(&name);
}

/// Reaps attestations with no live session (gc orphan sweep):
/// enumerates the state dir for attestation names, skips sessions
/// in `live_sessions`, removes the rest FD-relative. Returns
/// reaped file names for reporting. Unparseable names skip
/// (never a baseline comparison — name-attribution plus the
/// live-set gate is the safety, mirroring the tmpdir sweep).
pub fn reap_orphans(base: &Path, live_sessions: &std::collections::HashSet<String>) -> Vec<String> {
    let mut reaped = Vec::new();
    let state_dir = crate::lock::PrepareLease::state_dir(base);
    if !state_dir.exists() {
        return reaped;
    }
    let Ok(dir) = open_attestation_dir(base) else {
        return reaped;
    };
    let entries = match std::fs::read_dir(&state_dir) {
        Ok(entries) => entries,
        Err(_) => return reaped,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(session) = name.strip_prefix("cistella-attestation-") else {
            continue;
        };
        if session.is_empty()
            || !session
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            continue;
        }
        if live_sessions.contains(session) {
            continue;
        }
        dir.remove_file(&name);
        reaped.push(name);
    }
    reaped
}

/// Queries the live backend incarnation for one container name
/// via the existing `podman inspect` template pattern (cf. the
/// pidns proof). Failure means identity-unknown: the caller
/// refuses readiness, never proceeds.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when podman cannot report
/// both fields for the name.
pub fn query_incarnation(container_name: &str) -> Result<Incarnation> {
    let out = std::process::Command::new("podman")
        .args([
            "inspect",
            "--format",
            "{{.Id}} {{.State.StartedAt}}",
            container_name,
        ])
        .output()
        .map_err(|e| {
            CistellaError::Runtime(format!("incarnation inspect {container_name}: {e}"))
        })?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "incarnation inspect {container_name}: podman refused"
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let (Some(id), Some(started)) = (parts.next(), parts.next()) else {
        return Err(CistellaError::Runtime(format!(
            "incarnation inspect {container_name}: unexpected shape"
        )));
    };
    if id.is_empty() || started.is_empty() {
        return Err(CistellaError::Runtime(format!(
            "incarnation inspect {container_name}: empty identity"
        )));
    }
    Ok(Incarnation {
        container_id: id.to_string(),
        started_at: started.to_string(),
    })
}

/// Exact incarnation equality: the only comparison. Any field
/// difference (recreate → new ID, restart → new StartedAt) is
/// a stale generation, never a partial match.
#[must_use]
pub fn incarnation_matches(live: &Incarnation, attested: &Incarnation) -> bool {
    live == attested
}
