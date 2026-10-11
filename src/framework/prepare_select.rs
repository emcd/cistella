//! Extension selection for prepare: which guests answer transactions.
//!
//! Split from `prepare` at the file-size limit: the transaction
//! mechanics (host, hello, merge, shutdown) stay there; choosing
//! the spawns (none, one, or the landlock-plus-one pair) lives
//! here. Resolution inputs and lease bases arrive explicitly — no
//! environment reads beyond the lazy registry cache lookup for
//! registered entries.

use std::collections::HashSet;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{Deadlines, MergeContext, MountTriple, PreparePlan};
use crate::framework::guest::{GuestHost, host_external, host_registered};
use crate::framework::policy::{PolicySet, acceptance_set};
use crate::framework::prepare::{
    DRIVER_ENV, EXTENSION_BIN, EvaluatedPlan, ExtensionSpawn, check_hook_executable,
    merge_transacted, run_extension_prepare, spawn_invitation, transact_prepare,
};
use crate::guest_registry::HostEnv;
use crate::guest_source::{ResolveCtx, cache_dir, resolve};
use crate::lock::PrepareLease;
use crate::profile::ExtensionResolution;

/// Runs the Landlock extension prepare transaction: the shipped
/// guest through [`run_extension_prepare`]. Behavior is unchanged
/// (same offer, same required `guest-hooks` advertisement, same
/// merge) — this boundary keeps the production call site and the
/// existing conformance pins stable.
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
    session_id: &str,
    lease_base: &std::path::Path,
) -> Result<(EvaluatedPlan, Option<PrepareLease>)> {
    run_extension_prepare(
        ExtensionSpawn::Landlock,
        exe_dir,
        extra_args,
        profile,
        triples,
        policy,
        session_id,
        lease_base,
    )
}

/// One hosted extension guest plus its negotiated hello: the unit
/// the pair flow collects before transacting.
struct HostedOne {
    /// Live guest (shut down exactly once, by the owning flow).
    guest: GuestHost<std::process::ChildStdout, std::process::ChildStdin>,
    /// Negotiated capabilities (subset-checked at host).
    negotiated: Vec<String>,
    /// Provenance identity for the merge.
    guest_id: String,
    /// Required advertisements (checked pre-transact).
    required: Vec<String>,
}

/// Shuts hosted guests in reverse hosting order; the first residue
/// dominates (a clean shutdown preserves the incoming outcome).
fn shutdown_guests(hosted: Vec<HostedOne>) -> Option<CistellaError> {
    let mut residue = None;
    for mut one in hosted.into_iter().rev() {
        if residue.is_none()
            && let Err(error) = one.guest.shutdown()
        {
            residue = Some(error);
        }
    }
    residue
}

/// Shuts already-hosted guests after a mid-flow failure and folds
/// the outcome: clean shutdowns preserve the incoming error
/// (quiescence proven, lease drops); residue dominates and the
/// caller retains the uncertainty record.
fn abort_hosted(
    hosted: Vec<HostedOne>,
    lease: &mut PrepareLease,
    error: CistellaError,
) -> CistellaError {
    if shutdown_guests(hosted).is_some() {
        lease.retain();
    }
    error
}

/// Prepares the selected extensions (none, one, or the
/// landlock-plus-one pair): undeclared means the empty plan (pure
/// Podman behavior — no extension guest spawned); a single guest
/// runs through the generalized runner; the pair answers in
/// declaration order, concatenates raw plans, and merges once
/// centrally and atomically (collisions across plans refuse
/// deterministically under the same validators; hook order
/// follows declaration order). General N-extension composition
/// stays deferred: two non-landlock extensions refuse.
///
/// `extra_args` carries one argv slice per extension in order
/// (production passes empties; tests select peer modes per
/// guest); a short slice reads as empty for the tail.
///
/// Resolution inputs build lazily from the live seat environment
/// only for registered entries, so undeclared and shipped-only
/// conducts never touch cache environment shapes.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on admission, composition,
/// or merge refusal, `CistellaError::Protocol` on transport or
/// shutdown failure, and `CistellaError::Profile` on registry or
/// verification refusal. Eight parameters mirror the selection
/// inputs one-to-one; bundling would only move them.
#[allow(clippy::too_many_arguments)]
pub fn prepare_selected_extension(
    extensions: Vec<ExtensionResolution>,
    exe_dir: &std::path::Path,
    profile: &crate::profile::Profile,
    triples: &[MountTriple],
    policy: &PolicySet,
    session_id: &str,
    lease_base: &std::path::Path,
    extra_args: &[Vec<String>],
) -> Result<(EvaluatedPlan, Option<PrepareLease>)> {
    let non_landlock = extensions
        .iter()
        .filter(|entry| !matches!(entry, ExtensionResolution::Landlock))
        .count();
    if non_landlock > 1 {
        return Err(CistellaError::Profile(
            "at most one non-landlock extension per profile in v1".to_string(),
        ));
    }
    match extensions.len() {
        0 => Ok((
            EvaluatedPlan {
                merged: crate::framework::contract::MergedPlan {
                    environment: Vec::new(),
                    mounts: Vec::new(),
                    policy_claims: Vec::new(),
                    guest_hooks: Vec::new(),
                    cleanup: Vec::new(),
                },
                diagnostics: Vec::new(),
                credentials: Vec::new(),
            },
            None,
        )),
        1 => {
            let extra = extra_args.first().map(Vec::as_slice).unwrap_or(&[]);
            match extensions.into_iter().next() {
                Some(ExtensionResolution::Landlock) => run_extension_prepare(
                    ExtensionSpawn::Landlock,
                    exe_dir,
                    extra,
                    profile,
                    triples,
                    policy,
                    session_id,
                    lease_base,
                ),
                Some(ExtensionResolution::Registered(entry)) => {
                    let ctx = ResolveCtx::new(cache_dir(&HostEnv::live())?);
                    let resolved = resolve(&entry, &ctx)?;
                    run_extension_prepare(
                        ExtensionSpawn::Registered(resolved),
                        exe_dir,
                        extra,
                        profile,
                        triples,
                        policy,
                        session_id,
                        lease_base,
                    )
                }
                None => unreachable!("length checked as 1"),
            }
        }
        // The pair rule above guarantees exactly landlock plus one
        // other here (duplicates refuse at profile resolution, so
        // no third list reaches this arm).
        _ => prepare_pair(
            extensions, exe_dir, profile, triples, policy, session_id, lease_base, extra_args,
        ),
    }
}

/// Hosts and transacts the landlock-plus-one pair: each guest
/// answers in declaration order, raw plans concatenate, one
/// central merge stays atomic. Provenance for lattice evaluation
/// uses the first guest's identity (matching is variant-level —
/// every contribution here is extension-provenance either way).
#[allow(clippy::too_many_arguments)]
fn prepare_pair(
    extensions: Vec<ExtensionResolution>,
    exe_dir: &std::path::Path,
    profile: &crate::profile::Profile,
    triples: &[MountTriple],
    policy: &PolicySet,
    session_id: &str,
    lease_base: &std::path::Path,
    extra_args: &[Vec<String>],
) -> Result<(EvaluatedPlan, Option<PrepareLease>)> {
    let mut lease = PrepareLease::create(lease_base, session_id)?;
    // Resolve registered entries first (failures refuse pre-spawn
    // with no guest hosted yet — the lease drops unretained).
    let mut spawns = Vec::with_capacity(extensions.len());
    for entry in extensions {
        match entry {
            ExtensionResolution::Landlock => spawns.push(ExtensionSpawn::Landlock),
            ExtensionResolution::Registered(registered) => {
                let ctx = ResolveCtx::new(cache_dir(&HostEnv::live())?);
                let resolved = resolve(&registered, &ctx)?;
                spawns.push(ExtensionSpawn::Registered(resolved));
            }
        }
    }
    // Host in declaration order; a mid-loop failure shuts
    // previously hosted guests (clean shutdowns preserve the
    // original error and drop the lease; residue dominates and
    // retains it).
    let mut hosted: Vec<HostedOne> = Vec::with_capacity(spawns.len());
    for (index, spawn) in spawns.into_iter().enumerate() {
        let (guest_id, offered, required) = spawn_invitation(&spawn);
        let extra = extra_args.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let hosted_one = match spawn {
            ExtensionSpawn::Landlock => host_external(
                exe_dir,
                EXTENSION_BIN,
                extra,
                &offered,
                Deadlines::default(),
            ),
            ExtensionSpawn::Registered(resolved) => {
                host_registered(resolved, extra, &offered, Deadlines::default())
            }
        };
        match hosted_one {
            Ok((guest, negotiated)) => {
                if let Err(error) = lease.record_guest(guest.pid(), guest.pid()) {
                    lease.retain();
                    return Err(abort_hosted(hosted, &mut lease, error));
                }
                hosted.push(HostedOne {
                    guest,
                    negotiated,
                    guest_id,
                    required,
                });
            }
            Err(error) => return Err(abort_hosted(hosted, &mut lease, error)),
        }
    }
    // Required advertisements (pure check, no I/O).
    for one in &hosted {
        for required_capability in &one.required {
            if !one.negotiated.contains(required_capability) {
                let error = CistellaError::Contract(format!(
                    "extension guest {} must advertise {required_capability}",
                    one.guest_id
                ));
                return Err(abort_hosted(hosted, &mut lease, error));
            }
        }
    }
    // Transact each; concatenate raw plans in declaration order.
    let mut plan = PreparePlan {
        environment: Vec::new(),
        mounts: Vec::new(),
        policy_claims: Vec::new(),
        guest_hooks: Vec::new(),
        cleanup: Vec::new(),
    };
    let mut credentials = Vec::new();
    let mut capabilities: Vec<String> = Vec::new();
    let mut first_id: Option<String> = None;
    for one in hosted.iter_mut() {
        let outcome = transact_prepare(
            one.guest.exchange_mut(),
            &one.guest_id,
            Deadlines::default().plan,
            session_id,
        );
        match outcome {
            Ok((transacted, creds)) => {
                if first_id.is_none() {
                    first_id = Some(one.guest_id.clone());
                }
                plan.environment.extend(transacted.environment);
                plan.mounts.extend(transacted.mounts);
                plan.policy_claims.extend(transacted.policy_claims);
                plan.guest_hooks.extend(transacted.guest_hooks);
                plan.cleanup.extend(transacted.cleanup);
                credentials.extend(creds);
                capabilities.extend(one.negotiated.iter().cloned());
            }
            Err(error) => return Err(abort_hosted(hosted, &mut lease, error)),
        }
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
    let guest_id = first_id.expect("pair hosts at least one guest");
    let outcome = merge_transacted(
        plan,
        credentials,
        &guest_id,
        &capabilities,
        &context,
        policy,
        &acceptances,
    );
    // Shut down in reverse hosting order; residue dominates.
    if let Some(residue) = shutdown_guests(hosted) {
        lease.retain();
        return Err(residue);
    }
    let plan = outcome?;
    for hook in &plan.merged.guest_hooks {
        check_hook_executable(hook)?;
    }
    Ok((plan, Some(lease)))
}
