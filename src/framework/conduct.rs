//! Conduct-side profile evaluation through the policy lattice.
//!
//! Deliberate policy migration, not additive layering: this replaces
//! the legacy unconditional token-assignments veto
//! (`identity::assert_no_github_token_in_assignments`, removed at the
//! cutover). Token-shaped assignment names are now suppressible under
//! the compiled default, excusable only by exact-name user
//! acknowledgement; absence-by-default is preserved because the
//! unacknowledged default denial still refuses. Shipped 0.1.1
//! `environment-acceptances` stay grandfathered against compiled
//! defaults only; user rules take precedence over both.
//!
//! The seat-socket credential handle remains fixture-only:
//! no seat runtime dir is provisioned, so `credential_surface` keeps
//! its established path and is not admitted here.

use crate::error::Result;
use crate::framework::contract::Provenance;
use crate::framework::contract::{Deadlines, ReconciliationKey, UnitHandle};
use crate::framework::isolator::Isolator;
use crate::framework::policy::{PolicySet, acceptance_set, evaluate_all};
use crate::guest_registry::HostEnv;
use crate::guest_source::{ResolveCtx, cache_dir, resolve};
use crate::isolators::client::WireClient;
use crate::profile::{IsolatorResolution, Profile};

/// Evaluates conduct's fully resolved environment contributions
/// (assignment keys plus snapshotted acceptance names) against the
/// lattice as profile provenance with the profile's acceptance set
/// (grandfathering) and no transaction claims.
///
/// The slice carries keys plus values from `snapshot_acceptances`,
/// but only keys participate in the lattice (values are content-free
/// by construction); the slice shape stays for symmetry with the
/// snapshot API.
///
/// Call before unit creation: a refusal leaves no residue.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on the first refusing variable;
/// diagnostics name the variable, never its value.
pub fn evaluate_profile_contributions(
    profile: &Profile,
    accepted_env: &[(String, String)],
    policy: &PolicySet,
) -> Result<()> {
    let mut names = Vec::with_capacity(profile.environment_assignments.len() + accepted_env.len());
    for key in profile.environment_assignments.keys() {
        names.push((key.clone(), Provenance::Profile));
    }
    for (name, _) in accepted_env {
        names.push((name.clone(), Provenance::Profile));
    }
    let acceptances = acceptance_set(&profile.environment_acceptances);
    evaluate_all(policy, &names, &acceptances, &[])
}

/// Hosts the conduct isolator per the resolved selection: the
/// shipped Podman backend, or a verified registered entry (resolved
/// fresh on every host — including re-hosts — so each spawn
/// re-verifies).
///
/// # Errors
///
/// Returns on registry cache, resolution, verification, spawn,
/// hello, role, or rendezvous failure, before any unit exists.
pub fn host_isolator(
    exe_dir: &std::path::Path,
    rendezvous_dir: &std::path::Path,
    isolator: &IsolatorResolution,
) -> Result<WireClient> {
    match isolator {
        IsolatorResolution::Podman => {
            WireClient::host(exe_dir, rendezvous_dir, Deadlines::default())
        }
        IsolatorResolution::Registered(entry) => {
            let resolved = resolve(entry, &resolve_ctx()?)?;
            WireClient::host_registered(resolved, rendezvous_dir, Deadlines::default())
        }
    }
}

/// Builds source-resolution inputs from the live seat environment
/// (fetch-slot cache directory). Called only when a registered
/// guest actually resolves, so seats without a registry never pay
/// for (or fail on) cache environment shapes — absent-registry
/// behavior stays exactly 0.2.0.
fn resolve_ctx() -> Result<ResolveCtx> {
    Ok(ResolveCtx::new(cache_dir(&HostEnv::live())?))
}

/// Releases the wire client on conduct exit paths: orderly guest
/// shutdown plus rendezvous directory removal. Returns the close
/// outcome: error paths report it to stderr while keeping their
/// primary error (a release failure there is secondary, never
/// silent), and the success path fails on it — a residue-class
/// close failure dominates a clean harness. Rendezvous-dir litter
/// reports to stderr without failing (litter, not residue).
pub fn release_client(client: WireClient, rendezvous_dir: &std::path::Path) -> Result<()> {
    let outcome = client.close();
    if let Err(error) = std::fs::remove_dir(rendezvous_dir) {
        eprintln!("error: rendezvous cleanup: {error}");
    }
    outcome
}

/// Reports a release failure on an already-failing path: the
/// primary error stays the report, but guest-shutdown residue is
/// never concealed.
pub fn report_release(outcome: Result<()>) {
    if let Err(error) = outcome {
        eprintln!("error: guest release: {error}");
    }
}

/// Aborts a startup after a signal: converges installed residue and returns
/// 128+signal. The creation-window guard must already be dropped (the
/// isolator methods used here assume the caller held it where the moved
/// mechanics did). Cleanup is verified: residue left behind fails the
/// invocation (exit 1) instead of reporting a clean signal exit. Takes
/// the wire client by value so the guest shuts down orderly on every
/// abort path instead of orphaning.
pub fn abort_startup(
    client: WireClient,
    rendezvous_dir: &std::path::Path,
    handle: Option<&UnitHandle>,
    container_name: &str,
    session_id: &str,
    signum: i32,
    // Exit-code frame: returns the disposition instead of exiting,
    // so the caller's scope (notably staging guards) always drops.
) -> i32 {
    if let Some(unit) = handle {
        let key = ReconciliationKey::generate();
        let grace = Deadlines::default().terminate_grace;
        let _ = client.terminate(unit, grace, &key);
        let _ = client.remove(unit, &key);
    }
    // Early-abort conducts may have recorded cleanup declarations
    // (prepare succeeded, unit never installed): reap them here
    // since no teardown runs on this path. Litter reports without
    // failing the abort disposition.
    for litter in crate::cleanup::reap_cleanup(&crate::lock::runtime_base(), session_id) {
        eprintln!("warning: cleanup litter: {litter}");
    }
    // Read uncertainty BEFORE release consumes the client, then
    // fold the release outcome in: the guest may enter a fatal
    // path DURING release itself (after wire terminate/remove
    // succeeded), so a pre-release snapshot alone is stale by
    // construction and a failed close dominates the signal
    // disposition. The release line already names any recorded
    // failure; the code must not claim a clean signal exit.
    let uncertain_before = client.shutdown_uncertain();
    let release_outcome = release_client(client, rendezvous_dir);
    let release_failed = release_outcome.is_err();
    report_release(release_outcome);
    let residue_left = !crate::runtime::residue_gone(container_name);
    if residue_left {
        eprintln!("error: startup abort left residue for {container_name}");
    }
    crate::isolators::client::abort_exit_code(
        residue_left,
        uncertain_before,
        release_failed,
        signum,
    )
}
