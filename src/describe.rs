//! `describe state` backend: commitment/snapshot separation for ownerless reads.
//!
//! A fresh CLI process cannot observe the original conduct's
//! in-memory execution table, so this module never reports
//! `executing`: the snapshot derives from durable records plus
//! live backend facts, and current harness liveness stays
//! unclaimed. The pure residue/selection table lives here for
//! unit pins; orchestration (registry resolution, rendering)
//! stays with the CLI caller.

use std::collections::HashMap;

use crate::error::{CistellaError, Result};
use crate::framework::isolator::UnitSnapshot;
use crate::framework::lifecycle::LifecycleState;

/// Minted session-id shape: fixed length over the Crockford-lower
/// alphabet (mirrors `session::mint_session_id`). A no-match id
/// selector in exactly this shape is a full id (probe the derived
/// container name); anything else is a prefix (Selector error).
pub fn full_id_shape(id: &str) -> bool {
    id.len() == crate::session::MINTED_ID_LEN
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'h' | b'j'..=b'k' | b'm'..=b'n' | b'p'..=b't' | b'v'..=b'z'))
}

/// Native backend evidence for one container name (single pass).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeEvidence {
    /// Present in `podman ps --all` (any status).
    pub container_present: bool,
    /// Quadlet unit file present.
    pub unit_present: bool,
}

/// Outcome of the no-record full-id probe table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Live backend residue dominates registry absence: typed
    /// error naming the container (never an `absent` claim).
    Residue { container_name: String },
    /// Inert attestation residue only: `absent` with success.
    AbsentAttested,
    /// Nothing anywhere: `absent` with success (native scope —
    /// the probe queried local evidence only).
    AbsentClean,
}

/// Pure probe table: container-or-unit presence dominates;
/// attestation-only residue resolves Absent; all-absent resolves
/// Absent. Query failures never reach this function (they error
/// typed at the call site).
#[must_use]
pub fn probe_table(
    container_name: &str,
    evidence: &NativeEvidence,
    attestation_present: bool,
) -> ProbeOutcome {
    if evidence.container_present || evidence.unit_present {
        ProbeOutcome::Residue {
            container_name: container_name.to_string(),
        }
    } else if attestation_present {
        ProbeOutcome::AbsentAttested
    } else {
        ProbeOutcome::AbsentClean
    }
}

/// Collects native evidence for one container name. Backend
/// contact failures are typed observation errors, never silent
/// absence (a failed query never reads as "not there").
pub fn probe_native(container_name: &str) -> Result<NativeEvidence> {
    let container_present =
        crate::isolators::quadlet::container_exists(container_name).map_err(|e| {
            CistellaError::Runtime(format!("describe probe container {container_name}: {e}"))
        })?;
    let unit_present =
        crate::isolators::podman::unit_file_present(&format!("{container_name}.container"));
    Ok(NativeEvidence {
        container_present,
        unit_present,
    })
}

/// Reads systemd service props with failure semantics: manager
/// failure and unexpected shapes are observation errors, never
/// silent defaults.
pub fn service_props(container_name: &str) -> Result<HashMap<String, String>> {
    crate::isolators::quadlet::query_unit_props_checked(&format!("{container_name}.service"))
        .map_err(|e| {
            CistellaError::Runtime(format!("describe probe service {container_name}: {e}"))
        })
}

/// Guards the commitment≠currency invariant at the render
/// boundary: an ownerless snapshot must never carry
/// `executing` (a fresh backend holds no executions, so this
/// fires only on an internal logic break — fail loud, never
/// print a weakened enum).
pub fn snapshot_checked(snapshot: &UnitSnapshot) -> Result<LifecycleState> {
    match snapshot.lifecycle {
        LifecycleState::Executing => Err(CistellaError::Runtime(
            "describe refuses ownerless executing (internal invariant)".to_string(),
        )),
        state => Ok(state),
    }
}

/// Renders the verbatim lowercase lifecycle token.
#[must_use]
pub fn render_state(state: &LifecycleState) -> &'static str {
    match state {
        LifecycleState::Created => "created",
        LifecycleState::Initiated => "initiated",
        LifecycleState::Executing => "executing",
        LifecycleState::Stopped => "stopped",
        LifecycleState::Absent => "absent",
    }
}
