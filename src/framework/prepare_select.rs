//! Extension selection for prepare: which guests answer transactions.
//!
//! Split from `prepare` at the file-size limit: the transaction
//! mechanics (host, hello, merge, shutdown) stay there; choosing
//! the spawns (none, one, or many) lives here. Resolution inputs
//! and lease bases arrive explicitly — no
//! environment reads beyond the lazy registry cache lookup for
//! registered entries.

use std::collections::HashSet;

use crate::error::{CistellaError, Result};
use crate::framework::contract::Capability;
use crate::framework::contract::CapabilitySet;
use crate::framework::contract::PermissionContribution;
use crate::framework::contract::Provenance;
use crate::framework::contract::gate_capabilities;
use crate::framework::contract::merge_transacted_as;
use crate::framework::contract::parse_capability;
use crate::framework::contract::{Deadlines, MergeContext, MountTriple, PreparePlan};
use crate::framework::guest::{GuestHost, host_external, host_registered};
use crate::framework::policy::{PolicySet, acceptance_set};
use crate::framework::prepare::{
    DRIVER_ENV, EXTENSION_BIN, EvaluatedPlan, ExtensionSpawn, TMPDIR_BIN, check_hook_executable,
    run_extension_prepare, spawn_invitation, transact_prepare,
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
    // Every hosted guest shuts down, even past the first
    // residue: leaving a guest running to preserve an earlier
    // error would strand it. The first residue dominates.
    let mut residue = None;
    for mut one in hosted.into_iter().rev() {
        if let Err(error) = one.guest.shutdown()
            && residue.is_none()
        {
            residue = Some(error);
        }
    }
    residue
}

/// Shuts already-hosted guests after a mid-flow failure and folds
/// the outcome: clean shutdowns preserve the incoming error
/// (quiescence proven, lease drops); residue dominates the
/// incoming error and retains the uncertainty record.
fn abort_hosted(
    hosted: Vec<HostedOne>,
    lease: &mut PrepareLease,
    error: CistellaError,
) -> CistellaError {
    match shutdown_guests(hosted) {
        Some(residue) => {
            lease.retain();
            residue
        }
        None => error,
    }
}

/// Prepares the selected extensions (none, one, or many):
/// undeclared means the empty plan (pure Podman behavior — no
/// extension guest spawned); a single guest runs through the
/// generalized runner; any number answers in declaration order,
/// concatenates raw plans, and merges once centrally and
/// atomically (collisions across plans refuse deterministically
/// under the same validators; hook order follows declaration
/// order).
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
    match extensions.len() {
        0 => Ok((
            EvaluatedPlan {
                merged: crate::framework::contract::MergedPlan {
                    environment: Vec::new(),
                    placements: Vec::new(),
                    permissions: Vec::new(),
                    policy_claims: Vec::new(),
                    guest_hooks: Vec::new(),
                    cleanup: Vec::new(),
                },
                effective: crate::framework::contract::EffectivePermissions::default(),
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
                Some(ExtensionResolution::Tmpdir) => run_extension_prepare(
                    ExtensionSpawn::Tmpdir,
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
        // Any arity composes here (duplicates refuse at profile
        // resolution, so no list reaches this arm twice-named).
        _ => prepare_many(
            extensions, exe_dir, profile, triples, policy, session_id, lease_base, extra_args,
        ),
    }
}

/// Hosts and transacts N extensions: each guest answers in
/// declaration order, raw plans concatenate, one central merge
/// stays atomic. Provenance for lattice evaluation uses each
/// guest's own identity (matching is variant-level — every
/// contribution here is extension-provenance either way).
#[allow(clippy::too_many_arguments)]
fn prepare_many(
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
            ExtensionResolution::Tmpdir => spawns.push(ExtensionSpawn::Tmpdir),
            ExtensionResolution::Registered(registered) => {
                let ctx = ResolveCtx::new(cache_dir(&HostEnv::live())?);
                let resolved = resolve(&registered, &ctx)?;
                spawns.push(ExtensionSpawn::Registered(resolved));
            }
        }
    }
    // Host in declaration order; a pending marker precedes
    // each spawn (a crash between spawn and record leaves gc
    // skipping, never assuming). A mid-loop failure shuts
    // previously hosted guests (clean shutdowns preserve the
    // original error and drop the lease; residue dominates and
    // retains it); hosting failures always retain (the failed
    // host's shutdown is unprovable post-hoc — stale-gc heals).
    let mut hosted: Vec<HostedOne> = Vec::with_capacity(spawns.len());
    for (index, spawn) in spawns.into_iter().enumerate() {
        if let Err(error) = lease.mark_pending() {
            return Err(abort_hosted(hosted, &mut lease, error));
        }
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
            ExtensionSpawn::Tmpdir => {
                host_external(exe_dir, TMPDIR_BIN, extra, &offered, Deadlines::default())
            }
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
            Err(error) => {
                lease.retain();
                return Err(abort_hosted(hosted, &mut lease, error));
            }
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
    // Hook-set admission: at most one nonempty guest-hook set
    // containing at most one request. One guest returning two
    // hooks refuses exactly like two guests returning one each
    // (counting nonempty plans alone would not enforce the
    // compose singleton). Tracked incrementally; refusals name
    // all claimants pre-merge.
    let mut hook_claimant: Option<String> = None;
    // Transact each; concatenate raw plans in declaration order.
    let mut plan = PreparePlan {
        environment: Vec::new(),
        mounts: Vec::new(),
        permissions: Vec::new(),
        policy_claims: Vec::new(),
        guest_hooks: Vec::new(),
        cleanup: Vec::new(),
    };
    let mut credentials = Vec::new();
    let mut capabilities: Vec<String> = Vec::new();
    // Multimap through validation: every claimant recorded in
    // declaration order. Merged names are unique (collisions
    // refuse with all claimants named), so the lattice closure
    // reads the sole entry; nothing is ever last-writer-wins.
    let mut provenances: std::collections::HashMap<String, Vec<Provenance>> =
        std::collections::HashMap::new();
    for one in hosted.iter_mut() {
        // Own negotiated set first: every gate below authorizes
        // against it, so no guest borrows another's advertisement.
        let own = CapabilitySet::new(
            &one.negotiated
                .iter()
                .filter_map(|name| parse_capability(name))
                .collect::<Vec<_>>(),
        );
        let outcome = transact_prepare(
            one.guest.exchange_mut(),
            &one.guest_id,
            Deadlines::default().plan,
            session_id,
        );
        match outcome {
            Ok((transacted, creds)) => {
                // Per-plan capability gate BEFORE concatenation:
                // each raw plan authorizes only against its own
                // negotiated set, so one guest cannot launder a
                // contribution type through the other's
                // advertisement. The union gate at merge stays as
                // a second layer (types nobody advertised still
                // refuse).
                if let Err(error) = gate_capabilities(&transacted, &own) {
                    return Err(abort_hosted(hosted, &mut lease, error));
                }
                // Credentials gate on the same own set: handles
                // travel beside the plan, so an unadvertised
                // credential type must refuse here, not at the
                // later union gate where a sibling's advertisement
                // would cover it.
                if !creds.is_empty() && !own.allows(Capability::Credentials) {
                    return Err(abort_hosted(
                        hosted,
                        &mut lease,
                        CistellaError::Contract(
                            "guest returned unadvertised contribution type: credentials"
                                .to_string(),
                        ),
                    ));
                }
                // Guest-local claim scope: each claim must match a
                // name this same guest contributed in this same
                // transaction. A claim reaching only another
                // guest's names refuses here; the global partition
                // at merge re-checks as the second layer.
                let own_names: Vec<String> = transacted
                    .environment
                    .iter()
                    .map(|contribution| contribution.name.clone())
                    .collect();
                if let Err(error) = policy.partition_claims(&transacted.policy_claims, &own_names) {
                    return Err(abort_hosted(hosted, &mut lease, error));
                }
                // Single-set-single-request admission (messages
                // bind before the abort moves the hosted guests).
                if !transacted.guest_hooks.is_empty() {
                    if transacted.guest_hooks.len() > 1 {
                        let message = format!(
                            "extension guest {} returned {} hook requests: at most one",
                            one.guest_id,
                            transacted.guest_hooks.len()
                        );
                        return Err(abort_hosted(
                            hosted,
                            &mut lease,
                            CistellaError::Contract(message),
                        ));
                    }
                    if let Some(prior) = hook_claimant.replace(one.guest_id.clone()) {
                        let message = format!(
                            "more than one guest-hook claimant: {prior} and {}",
                            one.guest_id
                        );
                        return Err(abort_hosted(
                            hosted,
                            &mut lease,
                            CistellaError::Contract(message),
                        ));
                    }
                }
                for contribution in &transacted.environment {
                    provenances
                        .entry(contribution.name.clone())
                        .or_default()
                        .push(contribution.provenance.clone());
                }
                plan.environment.extend(transacted.environment);
                plan.mounts.extend(transacted.mounts);
                plan.permissions.extend(transacted.permissions);
                plan.policy_claims.extend(transacted.policy_claims);
                plan.guest_hooks.extend(transacted.guest_hooks);
                plan.cleanup.extend(transacted.cleanup);
                credentials.extend(creds);
                capabilities.extend(one.negotiated.iter().cloned());
            }
            Err(error) => return Err(abort_hosted(hosted, &mut lease, error)),
        }
    }
    // Attribution gate: no name enters the merge without a
    // recorded contributor. A missing entry is a typed internal
    // refusal, never a guessed identity (guessing would launder
    // provenance through the lattice).
    for contribution in &plan.environment {
        if !provenances.contains_key(&contribution.name) {
            return Err(abort_hosted(
                hosted,
                &mut lease,
                CistellaError::Contract(format!(
                    "internal: merged name {} has no recorded contributor",
                    contribution.name
                )),
            ));
        }
    }
    let mut reserved: HashSet<String> = profile.environment_assignments.keys().cloned().collect();
    for name in &profile.environment_acceptances {
        reserved.insert(name.clone());
    }
    for name in DRIVER_ENV {
        reserved.insert(name.to_string());
    }
    let profile_permissions: Vec<PermissionContribution> = profile
        .permissions
        .iter()
        .map(|entry| PermissionContribution {
            target: entry.path.clone(),
            rights: entry.rights,
            provenance: Provenance::Profile,
        })
        .collect();
    let context = MergeContext::new(&profile.container_home, reserved, triples.to_vec())
        .with_profile_permissions(profile_permissions);
    let acceptances = acceptance_set(&profile.environment_acceptances);
    // Per-name provenance: lattice evaluation sees the source
    // that contributed each name (unreachable fallback — merge
    // invents no names and duplicates refuse deterministically).
    let outcome = merge_transacted_as(
        plan,
        credentials,
        &|name: &str| {
            provenances
                .get(name)
                .and_then(|claimants| claimants.first().cloned())
                .unwrap_or_else(|| {
                    // Unreachable by construction (merge invents no
                    // names; every contributed name records its guest
                    // above and the pre-merge check below refuses any
                    // gap typed) — but a guess would launder
                    // provenance, so the fallback is impossible, not
                    // defaulted.
                    Provenance::Extension("impossible-missing-contributor".to_string())
                })
        },
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
