//! Extension selection for prepare: which guest answers one transaction.
//!
//! Split from `prepare` at the file-size limit: the transaction
//! mechanics (host, hello, merge, shutdown) stay there; choosing
//! the spawn (shipped Landlock vs resolved registered entry, or
//! none) lives here. Resolution inputs and lease bases arrive
//! explicitly — no environment reads beyond the lazy registry
//! cache lookup for registered entries.

use crate::error::Result;
use crate::framework::contract::MountTriple;
use crate::framework::policy::PolicySet;
use crate::framework::prepare::{EvaluatedPlan, ExtensionSpawn, run_extension_prepare};
use crate::guest_registry::HostEnv;
use crate::guest_source::{ResolveCtx, cache_dir, resolve};
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
) -> Result<(EvaluatedPlan, Option<crate::lock::PrepareLease>)> {
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

/// Prepares the single selected extension (or none): undeclared
/// means the empty plan (pure Podman behavior — no extension guest
/// spawned); shipped Landlock runs unchanged; a registered entry
/// resolves to verified bytes first (failures refuse pre-create),
/// then answers through the generalized runner with its recorded
/// role required and hooks optional.
///
/// Resolution inputs build lazily from the live seat environment
/// only for registered entries, so undeclared and shipped-only
/// conducts never touch cache environment shapes.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on admission or merge
/// refusal, `CistellaError::Protocol` on transport or shutdown
/// failure, and `CistellaError::Profile` on registry or
/// verification refusal.
pub fn prepare_selected_extension(
    extension: Option<ExtensionResolution>,
    exe_dir: &std::path::Path,
    profile: &crate::profile::Profile,
    triples: &[MountTriple],
    policy: &PolicySet,
    session_id: &str,
    lease_base: &std::path::Path,
) -> Result<(EvaluatedPlan, Option<crate::lock::PrepareLease>)> {
    match extension {
        None => Ok((
            EvaluatedPlan {
                merged: crate::framework::contract::MergedPlan {
                    environment: Vec::new(),
                    mounts: Vec::new(),
                    policy_claims: Vec::new(),
                    guest_hooks: Vec::new(),
                },
                diagnostics: Vec::new(),
                credentials: Vec::new(),
            },
            None,
        )),
        Some(ExtensionResolution::Landlock) => run_landlock_prepare(
            exe_dir,
            &[],
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
                &[],
                profile,
                triples,
                policy,
                session_id,
                lease_base,
            )
        }
    }
}
