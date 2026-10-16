//! Await-outcome corroboration: read-only lifecycle queries backing
//! the observational `Unobservable` outcome.
//!
//! Split from the Podman backend (line-count doctrine): table-name
//! resolution stays on the backend
//! ([`crate::isolators::podman::PodmanIsolator::corroboration_names`]);
//! everything here is backend-agnostic orchestration over those
//! names plus the pure [`classify_completion`] decision.

//! Await-outcome corroboration: read-only lifecycle queries backing
//! the observational `Unobservable` outcome, plus the backend-domain
//! completion decision.
//!
//! Split from the Podman backend (line-count doctrine): table-name
//! resolution stays on the backend
//! ([`crate::isolators::podman::PodmanIsolator::corroboration_names`]);
//! everything here is isolator-domain orchestration over those names
//! plus the pure [`classify_completion`] decision. The decision lives
//! here (not in `framework::isolator`) because it interprets
//! Podman container-exists codes and systemd state names —
//! backend-domain knowledge the framework must not own.

use crate::framework::contract::{CancelFlag, ExecutionHandle};
use crate::framework::isolator::{
    AbsenceObservation, ExecutionOutcome, RawClientResult, UnobservableOutcome,
};
use crate::framework::signals::conduct_signal;
use crate::isolators::podman::{PodmanIsolator, unit_file_present};
use crate::isolators::quadlet::{container_exists_code, query_unit_props};

/// Read-only lifecycle facts corroboration needs. Backend-domain
/// shape (Podman container-exists codes, systemd state names): all
/// fields are snapshot values recorded in query order (container,
/// service, unit); co-occurrence only, never proven cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorroborationFacts {
    /// `podman container exists` exit code (0 present, 1 absent).
    pub container_exists_code: i32,
    /// Systemd `ActiveState` at query time.
    pub service_active_state: String,
    /// Systemd `LoadState` at query time.
    pub service_load_state: String,
    /// Unit file present at query time.
    pub unit_file_present: bool,
}

/// Classifies one completed foreground exec-client wait.
///
/// `conduct_signal` carries the driver's own signals (already
/// resolved by the caller) and wins before any status reading
/// (unchanged legacy). A zero status reports `Exited(0)` with no
/// queries. Otherwise the caller supplies read-only corroboration
/// facts when obtainable. Container ABSENT (code 1) is a hard fact:
/// no container means no observable harness, so the outcome is
/// `Unobservable` regardless of service readings. Container present
/// but service NOT active (stop/interruption in flight — mere
/// existence covers stopped-but-present containers, so existence
/// alone cannot gate) is likewise `Unobservable`, with the recorded
/// states as the observation. Service active with container
/// present, query failure, or unresolvable names (`facts` None)
/// keep the legacy status mapping byte-for-byte by explicit
/// compatibility policy (receipt unproven, never upgraded) — two
/// outcome branches, no third unknown-variant.
#[must_use]
pub fn classify_completion(
    conduct_signal: Option<i32>,
    status: &std::process::ExitStatus,
    facts: Option<&CorroborationFacts>,
) -> ExecutionOutcome {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signum) = conduct_signal {
        return ExecutionOutcome::Signaled(signum);
    }
    if status.code() == Some(0) {
        return ExecutionOutcome::Exited(0);
    }
    let raw = match status.signal() {
        Some(signum) => RawClientResult::Signaled(signum),
        None => RawClientResult::Exited(status.code().unwrap_or(1)),
    };
    let unobservable = |facts: &CorroborationFacts| {
        ExecutionOutcome::Unobservable(UnobservableOutcome {
            observation: AbsenceObservation {
                container_exists_code: facts.container_exists_code,
                service_active_state: facts.service_active_state.clone(),
                service_load_state: facts.service_load_state.clone(),
                unit_file_present: facts.unit_file_present,
            },
            raw,
        })
    };
    match facts {
        Some(facts) if facts.container_exists_code == 1 => unobservable(facts),
        Some(facts)
            if !facts.service_active_state.is_empty() && facts.service_active_state != "active" =>
        {
            unobservable(facts)
        }
        _ => match raw {
            RawClientResult::Exited(code) => ExecutionOutcome::Exited(code),
            RawClientResult::Signaled(signum) => ExecutionOutcome::Signaled(signum),
        },
    }
}

/// Classifies one completed exec-client wait with read-only
/// lifecycle corroboration. Zero statuses report directly;
/// nonzero/signaled completions query container/service/unit state
/// at the await boundary (in that order); any query failure or
/// unresolvable name degrades to the legacy status mapping
/// (absence unproven — never claimed).
pub(crate) fn corroborated_outcome(
    backend: &PodmanIsolator,
    execution: &ExecutionHandle,
    status: &std::process::ExitStatus,
    cancel: &CancelFlag,
) -> ExecutionOutcome {
    if let Some(signum) = conduct_signal(cancel) {
        return ExecutionOutcome::Signaled(signum);
    }
    if status.code() == Some(0) {
        return ExecutionOutcome::Exited(0);
    }
    let facts = corroboration_facts(backend, execution);
    classify_completion(None, status, facts.as_ref())
}

/// Gathers corroboration facts: names resolve through the backend
/// tables, then read-only state queries run container → service →
/// unit. Returns `None` when names do not resolve or any query
/// fails — the caller then keeps legacy mapping.
fn corroboration_facts(
    backend: &PodmanIsolator,
    execution: &ExecutionHandle,
) -> Option<CorroborationFacts> {
    let (container_name, unit_name) = backend.corroboration_names(execution)?;
    let code = container_exists_code(&container_name).ok()?;
    if code != 0 && code != 1 {
        return None;
    }
    let service = format!("{container_name}.service");
    let props = query_unit_props(&service);
    Some(CorroborationFacts {
        container_exists_code: code,
        service_active_state: props.get("ActiveState").cloned().unwrap_or_default(),
        service_load_state: props.get("LoadState").cloned().unwrap_or_default(),
        unit_file_present: unit_file_present(&unit_name),
    })
}

/// Final absence proof: re-queried AFTER teardown and guest
/// release, never inferred from earlier snapshots. The backend
/// convergence rule lives in [`AbsenceProof::converged_facts`] beside
/// the queries — the framework renders the bit, never the states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsenceProof {
    /// `podman container exists` exit code (0 present, 1 absent).
    pub container_exists_code: i32,
    /// Systemd `ActiveState` at proof time.
    pub service_active_state: String,
    /// Systemd `LoadState` at proof time.
    pub service_load_state: String,
    /// Unit file absent at proof time.
    pub unit_absent: bool,
    /// Converged per the backend rule below.
    pub converged: bool,
}

impl AbsenceProof {
    /// Backend convergence rule: container strictly absent (exit 1
    /// only) AND service not active AND unit file absent. Empty
    /// service strings (query unavailable) pass that clause — the
    /// load-bearing facts are container-absent + unit-absent — but
    /// are recorded as-is for the report. A stopped-but-present
    /// container never converges (residue until reaped).
    /// Public so the rule pins directly against fabricated facts;
    /// `prove_absence` below is the querying constructor.
    #[must_use]
    pub fn new(
        container_exists_code: i32,
        service_active_state: String,
        service_load_state: String,
        unit_absent: bool,
    ) -> Self {
        let converged =
            container_exists_code == 1 && service_active_state != "active" && unit_absent;
        Self {
            container_exists_code,
            service_active_state,
            service_load_state,
            unit_absent,
            converged,
        }
    }
}

/// Re-proves absence after teardown and release: container, service,
/// unit queried in that order. The unit name derives from the
/// container by the backend's own convention (`{container}.container`,
/// as minted in `adopt`). Any query failure is an observation
/// error (returned as `Err`), never silent convergence.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on spawn failure or any
/// non-0/1 container-exists code.
pub(crate) fn prove_absence(container_name: &str) -> crate::error::Result<AbsenceProof> {
    // Strict codes flow from `container_exists_code`: 1 is absent,
    // anything else errors (an observation error, never convergence).
    let code = container_exists_code(container_name)?;
    let service = format!("{container_name}.service");
    let props = query_unit_props(&service);
    let unit_name = format!("{container_name}.container");
    let unit_absent = !unit_file_present(&unit_name);
    Ok(AbsenceProof::new(
        code,
        props.get("ActiveState").cloned().unwrap_or_default(),
        props.get("LoadState").cloned().unwrap_or_default(),
        unit_absent,
    ))
}
