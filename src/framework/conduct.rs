//! Conduct-side profile evaluation through the policy lattice
//! (task 4.1).
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
//! The seat-socket credential handle is fixture-only in this slice:
//! no seat runtime dir is provisioned, so `credential_surface` keeps
//! its established path and is not admitted here.

use crate::error::Result;
use crate::framework::contract::Provenance;
use crate::framework::policy::{PolicySet, acceptance_set, evaluate_all};
use crate::profile::Profile;

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
/// Call before unit/scratch creation: a refusal leaves no residue.
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
