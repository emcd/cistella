//! Await-outcome corroboration: read-only lifecycle queries backing
//! the observational `Unobservable` outcome.
//!
//! Split from the Podman backend (line-count doctrine): table-name
//! resolution stays on the backend
//! ([`crate::isolators::podman::PodmanIsolator::corroboration_names`]);
//! everything here is backend-agnostic orchestration over those
//! names plus the pure [`classify_completion`] decision.

use crate::framework::contract::{CancelFlag, ExecutionHandle};
use crate::framework::isolator::{CorroborationFacts, ExecutionOutcome, classify_completion};
use crate::framework::signals;
use crate::isolators::podman::{PodmanIsolator, unit_file_present};
use crate::isolators::quadlet::{container_exists_code, query_unit_props};

/// Driver's own signals win before any status reading (unchanged
/// legacy order): an explicit cancel, HUP, or TERM belongs to
/// conduct, never to the harness.
fn conduct_signal(cancel: &CancelFlag) -> Option<i32> {
    if let Some(signum) = cancel.signum() {
        return Some(signum);
    }
    if signals::got_hup() {
        return Some(1);
    }
    if signals::got_term() {
        return Some(15);
    }
    None
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
