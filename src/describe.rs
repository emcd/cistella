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

/// Exec-admittability evidence for one container name: single
/// `podman inspect` round trip plus unit-file presence. Absent
/// containers read as non-running evidence; backend contact
/// failures are typed errors, never silent absence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admittability {
    /// Container status is exactly `running`.
    pub running: bool,
    /// Container init PID (`State.Pid`;exec-admissibility proof).
    pub init_pid: i64,
    /// Quadlet unit file present.
    pub unit_present: bool,
}

/// Collects exec-admittability evidence for one container name.
pub fn exec_admittability(container_name: &str) -> Result<Admittability> {
    let out = std::process::Command::new("podman")
        .args([
            "inspect",
            "--format",
            "{{.State.Status}} {{.State.Pid}}",
            container_name,
        ])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("enter probe {container_name}: {e}")))?;
    let (running, init_pid) = if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.split_whitespace();
        let status = parts.next().unwrap_or_default();
        let pid: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
        (status == "running", pid)
    } else if crate::isolators::quadlet::container_exists(container_name).map_err(|e| {
        CistellaError::Runtime(format!("enter probe container {container_name}: {e}"))
    })? {
        return Err(CistellaError::Runtime(format!(
            "enter probe {container_name} failed while container exists"
        )));
    } else {
        (false, 0)
    };
    let unit_present =
        crate::isolators::podman::unit_file_present(&format!("{container_name}.container"));
    Ok(Admittability {
        running,
        init_pid,
        unit_present,
    })
}

/// Live-vs-bound incarnation comparison outcome (the caller
/// queries live identity via `attestation::query_incarnation`
/// and compares exactly; query failure is `Unknown`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveIncarnation {
    /// Live identity equals the bound identity.
    Current,
    /// Live identity differs (recreate or restart).
    Stale,
    /// Live identity unqueryable (refuse, never proceed).
    Unknown,
}

/// Enter routing decision: the pure decision table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterDecision {
    /// All edge conditions hold: proceed to exec.
    ProceedNative,
    /// Foreign commitment: legacy direct exec, no gate, no claim.
    ProceedLegacyDirect,
    /// Refuse immediately, naming the unmet condition.
    Refuse(String),
}

/// Pure enter-edge decision over a resolved record's
/// attestation (if any), the record's container name, the
/// live-vs-bound comparison, and admittability evidence.
/// Every refusal names its condition; no retries, no polls —
/// the caller renders `Refuse` as a typed error.
#[must_use]
pub fn decide_enter(
    attestation: Option<&crate::attestation::Attestation>,
    container_name: &str,
    live: &LiveIncarnation,
    admittability: &Admittability,
) -> EnterDecision {
    let Some(bound) = attestation else {
        return EnterDecision::Refuse("no launch attestation committed".to_string());
    };
    if bound.container_name != container_name {
        return EnterDecision::Refuse(
            "attestation container mismatch (possible out-of-band recreate)".to_string(),
        );
    }
    if bound.incarnation.is_none() {
        return EnterDecision::ProceedLegacyDirect;
    }
    match live {
        LiveIncarnation::Unknown => {
            return EnterDecision::Refuse(
                "launch incarnation unestablishable (refusing)".to_string(),
            );
        }
        LiveIncarnation::Stale => {
            return EnterDecision::Refuse(
                "stale launch incarnation (container recreated or restarted)".to_string(),
            );
        }
        LiveIncarnation::Current => {}
    }
    if !admittability.unit_present {
        return EnterDecision::Refuse("unit file absent".to_string());
    }
    if !admittability.running {
        return EnterDecision::Refuse("container not running".to_string());
    }
    if admittability.init_pid <= 0 {
        return EnterDecision::Refuse("container init PID not established".to_string());
    }
    EnterDecision::ProceedNative
}

/// Renders the verbatim lowercase lifecycle token for a
/// checked snapshot. `executing` is structurally unreachable
/// (only `snapshot_checked` constructs the input invariant —
/// the arm documents the impossibility, never a rendering).
#[must_use]
pub fn render_state(state: &LifecycleState) -> &'static str {
    match state {
        LifecycleState::Created => "created",
        LifecycleState::Initiated => "initiated",
        LifecycleState::Executing => {
            unreachable!("snapshot_checked excludes ownerless executing")
        }
        LifecycleState::Stopped => "stopped",
        LifecycleState::Absent => "absent",
    }
}
