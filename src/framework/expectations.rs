//! Declared-bundle enforcement: host-origin expected contribution
//! shapes, held against untrusted guest plans at merge time.
//!
//! Split from `contract` at the file-size limit: the merge keeps
//! the call sites and the reservation exemption; the bundle
//! definitions and their exactness checks live here. Generic
//! mechanism, not guest-specific.

use crate::error::{CistellaError, Result};
use crate::framework::contract::{MergeContext, PreparePlan, Provenance, Rights};

/// One host-declared expected bundle: the exact contribution
/// shape a named instance must return. Generic mechanism, not
/// guest-specific: the host declares what it set up (argv,
/// ambient, selection), and the merge holds the guest to it.
/// Diagnostics name variables and kinds, never values or paths.
#[derive(Debug, Clone)]
pub struct InstanceExpectation {
    /// Instance the bundle binds (`shipped:*`/`registered:*`).
    pub owner: Provenance,
    /// Exact `(name, value)` env pairs expected.
    pub env: Vec<(String, String)>,
    /// Exact `(host_source, container_target)` mounts expected.
    pub mounts: Vec<(String, String)>,
    /// Exact `(target, rights)` permissions expected.
    pub permissions: Vec<(String, Rights)>,
    /// Exact `(kind, locator-path)` credential handles expected
    /// (kinds plus counts checked at merge; path equality
    /// checked at admission, where content is still visible).
    pub credentials: Vec<(String, String)>,
}

/// Checks declared-bundle credential kinds plus counts against
/// admitted handles: each expectation's owner must hold exactly
/// the declared handle kinds. Path equality was enforced at
/// admission, where content is still visible; admitted handles
/// retain kind plus owner only — never content — so counts are
/// checked here. Diagnostics name kinds, never locators.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on omission, divergence, or
/// extras.
pub fn check_expected_credentials(
    credentials: &[crate::framework::credentials::AdmittedCredential],
    expectations: &[InstanceExpectation],
) -> Result<()> {
    for expected in expectations {
        let mut held: Vec<&str> = credentials
            .iter()
            .filter(|handle| handle.owner == expected.owner)
            .map(|handle| handle.kind)
            .collect();
        let mut declared: Vec<&str> = expected
            .credentials
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect();
        held.sort();
        declared.sort();
        if held != declared {
            return Err(CistellaError::Contract(
                "declared bundle credential mismatch: omission, divergence, or extras refuse"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// Enforces host-declared expected bundles against one raw plan:
/// each expectation's owner must contribute exactly the declared
/// env pairs, mount pairs, and permission pairs — and nothing
/// else of any contribution type (environment, mounts,
/// permissions, policy claims, hooks, cleanup). Omission,
/// divergence, and extras all refuse whole, so a buggy
/// empty/partial plan fails here instead of leaving its owned
/// name open. Credential kinds plus counts are enforced by the
/// caller holding the admitted handles; path equality is
/// enforced at admission, where content is still visible.
/// Diagnostics name variables and kinds, never values or paths.
pub(crate) fn check_expectations(plan: &PreparePlan, context: &MergeContext) -> Result<()> {
    for expected in &context.expectations {
        let mut env: Vec<(&str, &str)> = Vec::new();
        let mut mounts: Vec<(&str, &str)> = Vec::new();
        let mut permissions: Vec<(&str, Rights)> = Vec::new();
        for contribution in &plan.environment {
            if contribution.provenance == expected.owner {
                env.push((contribution.name.as_str(), contribution.value.as_str()));
            }
        }
        for contribution in &plan.mounts {
            if contribution.provenance == expected.owner {
                mounts.push((
                    contribution.placement.host_source.as_str(),
                    contribution.placement.container_target.as_str(),
                ));
            }
        }
        for contribution in &plan.permissions {
            if contribution.provenance == expected.owner {
                permissions.push((contribution.target.as_str(), contribution.rights));
            }
        }
        let mut expected_env: Vec<(&str, &str)> = expected
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let mut expected_mounts: Vec<(&str, &str)> = expected
            .mounts
            .iter()
            .map(|(source, target)| (source.as_str(), target.as_str()))
            .collect();
        let mut expected_permissions: Vec<(&str, Rights)> = expected
            .permissions
            .iter()
            .map(|(target, rights)| (target.as_str(), *rights))
            .collect();
        env.sort();
        expected_env.sort();
        mounts.sort();
        expected_mounts.sort();
        // Rights has no total order: sort by target, then by a
        // stable discriminant. Targets are unique per owner
        // (duplicates refuse at merge), so the target order
        // decides; the discriminant breaks same-target ties.
        fn rights_rank(rights: Rights) -> u8 {
            match rights {
                Rights::Read => 0,
                Rights::Write => 1,
                Rights::Full => 2,
            }
        }
        permissions.sort_by(|a, b| {
            a.0.cmp(b.0)
                .then_with(|| rights_rank(a.1).cmp(&rights_rank(b.1)))
        });
        expected_permissions.sort_by(|a, b| {
            a.0.cmp(b.0)
                .then_with(|| rights_rank(a.1).cmp(&rights_rank(b.1)))
        });
        if env != expected_env || mounts != expected_mounts || permissions != expected_permissions {
            return Err(CistellaError::Contract(
                "declared contribution bundle mismatch: omission, divergence, or extras refuse"
                    .to_string(),
            ));
        }
        let claims_extra = plan
            .policy_claims
            .iter()
            .any(|claim| claim.provenance == expected.owner);
        // Hooks and cleanup carry no owner provenance at merge
        // (raw wire passthrough): their emptiness is enforced
        // per-guest at transact time, where attribution is
        // exact — never here, where a sibling's declarations
        // would false-positive.
        if claims_extra {
            return Err(CistellaError::Contract(
                "declared bundle forbids policy claims".to_string(),
            ));
        }
    }
    Ok(())
}
