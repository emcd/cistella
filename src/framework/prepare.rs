//! Prepare transaction over the protocol exchange (task 2.2).
//!
//! One `prepare` exchange per session per extension: the guest
//! returns separately-typed environment and mount sets, policy
//! claims, and guest-hook requests; the host validates each set with
//! its existing rules, merges centrally and atomically, partitions
//! claims against the lattice (weakenings discarded with a typed
//! diagnostic, overreach refusing the whole transaction), and
//! evaluates every contributed name. Extension output is untrusted
//! input at every step.
//!
//! Conduct wiring rides the dogfood gate (task 4.1): this module
//! delivers the mechanism; fleet seats stay on their current path
//! until the unchanged-seat proof passes.

use std::collections::HashSet;
use std::io::Read;
use std::os::fd::AsFd;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{
    Capability, CapabilitySet, Deadlines, EnvContribution, GuestHookRequest, MergeContext,
    MountContribution, MountMode, MountTriple, PolicyClaim, PreparePlan, Provenance, merge_prepare,
};
use crate::framework::credentials::{AdmittedCredential, CredentialHandle, admit_all};
use crate::framework::guest::host_external;
use crate::framework::policy::{PolicySet, acceptance_set};
use crate::framework::protocol::Exchange;

/// Wire form of one environment contribution (provenance is injected
/// by the host as the responding guest, never trusted from the wire).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvWire {
    /// Exact variable name.
    name: String,
    /// Opaque value.
    value: String,
}

/// Wire form of one mount contribution.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MountWire {
    /// Host path.
    host_source: String,
    /// Container path.
    container_target: String,
    /// `ro` or `rw`.
    mode: String,
}

/// Wire form of the single prepare response.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareResponse {
    /// Environment contributions (default empty).
    #[serde(default)]
    environment: Vec<EnvWire>,
    /// Mount contributions (default empty).
    #[serde(default)]
    mounts: Vec<MountWire>,
    /// Policy claims scoped to this transaction (default empty).
    #[serde(default)]
    policy_claims: Vec<PolicyClaim>,
    /// Guest-hook requests (default empty).
    #[serde(default)]
    guest_hooks: Vec<GuestHookRequest>,
    /// Credential handles, variants only, never values (default empty).
    #[serde(default)]
    credentials: Vec<CredentialHandle>,
}

/// Evaluated plan: merged contributions plus claim diagnostics.
///
/// `diagnostics` records discarded weakenings (name-only); the merged
/// plan is whole or the transaction refused — partial application
/// never occurs. Admitted credential handles ride alongside for the
/// dogfood gate to consume.
#[derive(Debug, Clone)]
pub struct EvaluatedPlan {
    /// Centrally merged, policy-admitted plan.
    pub merged: crate::framework::contract::MergedPlan,
    /// Weakening-discard notes from claim partition.
    pub diagnostics: Vec<String>,
    /// Admitted credential handles (kinds + locator classes only).
    pub credentials: Vec<AdmittedCredential>,
}

/// Parses a capability advertisement name.
///
/// Unknown names are ignored (forward-compatible strictness lives in
/// the merge gate: undeclared contribution TYPES refuse, unknown
/// advertisement STRINGS do not).
#[must_use]
pub fn parse_capability(name: &str) -> Option<Capability> {
    match name {
        "environment" => Some(Capability::Environment),
        "mounts" => Some(Capability::Mounts),
        "policy-claims" => Some(Capability::PolicyClaims),
        "guest-hooks" => Some(Capability::GuestHooks),
        "credentials" => Some(Capability::Credentials),
        _ => None,
    }
}

/// Runs one prepare transaction against a guest exchange.
///
/// Sends `prepare`, parses the single response with unknown-field
/// refusal, gates contribution types against the negotiated
/// capabilities, merges atomically against the framework-owned
/// baseline context (destination collisions refuse), partitions
/// claims, and evaluates every contributed name against the lattice
/// (user acknowledgements and shipped-acceptance grandfathering
/// apply). At most one call per session per extension.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on transport, correlation, or
/// shape failures, and `CistellaError::Contract` on merge, claim, or
/// policy refusals. Any refusal rejects the whole transaction.
pub fn run_prepare<R: Read + AsFd, W: std::io::Write + AsFd>(
    exchange: &mut Exchange<R, W>,
    guest_id: &str,
    capabilities: &[String],
    context: &MergeContext,
    policy: &PolicySet,
    acceptances: &HashSet<String>,
    timeout: Duration,
) -> Result<EvaluatedPlan> {
    let payload = exchange.request(
        "prepare",
        serde_json::Value::Object(serde_json::Map::new()),
        timeout,
    )?;
    let response: PrepareResponse = serde_json::from_value(payload).map_err(|_| {
        CistellaError::Contract("bad prepare response: shape violation".to_string())
    })?;
    let provenance = Provenance::Extension(guest_id.to_string());
    let advertised = CapabilitySet::new(
        &capabilities
            .iter()
            .filter_map(|name| parse_capability(name))
            .collect::<Vec<_>>(),
    );
    if !response.credentials.is_empty() && !advertised.allows(Capability::Credentials) {
        return Err(CistellaError::Contract(
            "guest returned unadvertised contribution type: credentials".to_string(),
        ));
    }
    let credentials = admit_all(&response.credentials)?;
    let plan = build_plan(response, &provenance)?;
    let merged = merge_prepare(plan, &advertised, context)?;
    let contributed: Vec<String> = merged
        .environment
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let (claims, diagnostics) = policy.partition_claims(&merged.policy_claims, &contributed)?;
    for (name, _) in &merged.environment {
        policy.evaluate(name, &provenance, acceptances, &claims)?;
    }
    Ok(EvaluatedPlan {
        merged,
        diagnostics,
        credentials,
    })
}

/// Extension binary name resolved sibling-relative to the driver.
pub const EXTENSION_BIN: &str = "cistella-extension-landlock";

/// Role capability offered to the extension guest.
const EXTENSION_ROLE: &str = "landlock";

/// Contribution capability the Landlock hook exercises.
const EXTENSION_HOOKS: &str = "guest-hooks";

/// Driver-injected env names reserved against extension
/// contributions (unit-baked HOME, ssh-agent pointer, exec-time TERM).
const DRIVER_ENV: [&str; 3] = ["HOME", "SSH_AUTH_SOCK", "TERM"];

/// Runs the Landlock extension prepare transaction (task 3.1):
/// hosts the real extension guest per-phase, admits on the
/// guest-hooks contribution, merges centrally against the
/// framework-owned baseline, and shuts the guest down. Hook staging
/// and execution ride task 3.2; hooks return staged for the caller
/// to hold.
///
/// `extra_args` rides the guest spawn (production passes none;
/// tests select deterministic peer modes).
///
/// Admission checks the contribution type, not the role string:
/// the pinned binary name plus the install-directory trust anchor
/// already carry role identity, and only the hook contribution
/// gates confinement behavior.
///
/// Residue dominance: a failed shutdown replaces the prepare
/// outcome; only a clean shutdown preserves it.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on discovery, admission, or
/// merge refusal, and `CistellaError::Protocol` on transport or
/// shutdown failure.
pub fn run_landlock_prepare(
    exe_dir: &std::path::Path,
    extra_args: &[String],
    profile: &crate::profile::Profile,
    triples: &[MountTriple],
    policy: &PolicySet,
) -> Result<EvaluatedPlan> {
    let offered: Vec<String> = [EXTENSION_ROLE, EXTENSION_HOOKS]
        .iter()
        .map(|name| name.to_string())
        .collect();
    let (mut guest, negotiated) = host_external(
        exe_dir,
        EXTENSION_BIN,
        extra_args,
        &offered,
        Deadlines::default(),
    )?;
    if !negotiated.contains(&EXTENSION_HOOKS.to_string()) {
        guest.shutdown()?;
        return Err(CistellaError::Contract(
            "extension guest must advertise guest-hooks".to_string(),
        ));
    }
    let mut reserved: HashSet<String> = profile.environment_assignments.keys().cloned().collect();
    for name in &profile.environment_acceptances {
        reserved.insert(name.clone());
    }
    for name in DRIVER_ENV {
        reserved.insert(name.to_string());
    }
    let context = MergeContext::new(&profile.container_home, reserved, triples.to_vec());
    let acceptances = acceptance_set(&profile.environment_acceptances);
    let outcome = run_prepare(
        guest.exchange_mut(),
        EXTENSION_BIN,
        &negotiated,
        &context,
        policy,
        &acceptances,
        Deadlines::default().plan,
    );
    let shutdown = guest.shutdown();
    match (outcome, shutdown) {
        (Ok(plan), Ok(())) => Ok(plan),
        (_, Err(residue)) => Err(residue),
        (Err(error), Ok(())) => Err(error),
    }
}

/// Builds the typed plan from wire shapes (shape checks only; merge
/// and policy own semantics).
///
/// # Errors
///
/// Returns `CistellaError::Contract` on bad mount modes.
fn build_plan(response: PrepareResponse, provenance: &Provenance) -> Result<PreparePlan> {
    let mut mounts = Vec::with_capacity(response.mounts.len());
    for wire in &response.mounts {
        mounts.push(MountContribution {
            triple: MountTriple {
                host_source: wire.host_source.clone(),
                container_target: wire.container_target.clone(),
                mode: MountMode::parse_flag(&wire.mode)
                    .map_err(|e| CistellaError::Contract(format!("bad mount mode: {e}")))?,
            },
            provenance: provenance.clone(),
        });
    }
    Ok(PreparePlan {
        environment: response
            .environment
            .iter()
            .map(|wire| EnvContribution {
                name: wire.name.clone(),
                value: wire.value.clone(),
                provenance: provenance.clone(),
            })
            .collect(),
        mounts,
        policy_claims: response.policy_claims,
        guest_hooks: response.guest_hooks,
    })
}
