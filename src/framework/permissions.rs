//! Extension rights and effective permissions: the policy half
//! of the mount/permission split.
//!
//! Rights lattice, permission contributions, joined effective
//! grants, the single-source-of-truth map, and the one rendering
//! chokepoint (placements to binding triples). Split from
//! `contract` at the file-size limit; the merge pipeline stays
//! there.

use crate::error::{CistellaError, Result};
use crate::framework::contract::Provenance;
use crate::mount::{MountMode, MountTriple, Placement, canonicalize_container_target};
use serde::{Deserialize, Serialize};

/// Declared access right: the policy half of a former mode.
///
/// Rights outlive backends (Landlock today, something else
/// tomorrow); mount modes describe Podman bindings. `Full` is
/// exactly `Read ∪ Write` under join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rights {
    /// Read/execute (ancestor mask: EXECUTE + READ_FILE + READ_DIR).
    Read,
    /// Write (normalizes to effective `Full` at map construction:
    /// this fleet's policy grants the full matrix for write-capable
    /// rules since the session baseline already reads broadly).
    Write,
    /// Full matrix (bits 0–14 including TRUNCATE).
    Full,
}

impl Rights {
    /// Parses a wire rights string (`read`/`write`/`full`).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on anything else.
    pub fn parse_wire(s: &str) -> crate::error::Result<Self> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "full" => Ok(Self::Full),
            other => Err(crate::error::CistellaError::Contract(format!(
                "bad permission rights: {other}"
            ))),
        }
    }

    /// Lattice join: `Full` dominates, `Read ∪ Write` is `Full`.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Full, _) | (_, Self::Full) => Self::Full,
            (Self::Read, Self::Read) => Self::Read,
            _ => Self::Full,
        }
    }

    /// True for write-capable rights (`Write` or `Full`).
    #[must_use]
    pub fn writes(self) -> bool {
        !matches!(self, Self::Read)
    }
}

/// One permission contribution: rights on a guest-visible target.
///
/// A permission is addressable iff its target matches a placement
/// contributed by any guest in the same session, a driver target,
/// or a known image-private baseline target; anything else refuses
/// as an unknown grant target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionContribution {
    /// Guest-visible target path.
    pub target: String,
    /// Declared rights (minimum grant; `Write` normalizes later).
    pub rights: Rights,
    /// Contribution source for policy scoping.
    pub provenance: Provenance,
}

/// Effective grant for one canonical target: joined minimum rights
/// plus the surviving binding intent.
///
/// Grants join (least upper bound of minima) while binding intent
/// persists separately: read-only if ANY source declared
/// read-only, so a join can widen the Landlock grant without
/// erasing the VFS wall retention reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveGrant {
    /// Canonical container target this grant covers.
    pub target: String,
    /// Joined rights (`Write` normalized to `Full` at construction).
    pub rights: Rights,
    /// Every claimant with its original declared label.
    pub claimants: Vec<(Provenance, Rights)>,
    /// True when any source declared read-only (driver ro,
    /// extension `Read`); retention and revision read this.
    pub binding_intent_ro: bool,
}

/// Post-merge effective permissions: the single source of truth
/// every consumer (grant computation, alias guards, retention,
/// descendant checks, rendering, revision, supervisor) reads.
#[derive(Debug, Clone, Default)]
pub struct EffectivePermissions {
    /// Joined grants keyed by canonical container target.
    grants: std::collections::HashMap<String, EffectiveGrant>,
}

impl EffectivePermissions {
    /// Assembles the map from driver triples (mode implies policy:
    /// ro→`Read` + RO intent, rw→`Full` + writable intent),
    /// contributed placements, and extension permission
    /// contributions (joined by canonical target with all
    /// provenances recorded; `Write` normalizes to `Full` before
    /// every consumer).
    ///
    /// Refuses typed on an unknown permission target (matches no
    /// contributed placement, no driver target, and no known
    /// baseline), a contributed placement with no matching
    /// permission (no silent default), and an exact-duplicate
    /// permission (same target, provenance, and rights).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on any of the above.
    pub fn assemble(
        driver_triples: &[MountTriple],
        placements: &[Placement],
        permissions: &[PermissionContribution],
        baselines: &[String],
    ) -> Result<Self> {
        let mut grants: std::collections::HashMap<String, EffectiveGrant> =
            std::collections::HashMap::new();
        for triple in driver_triples {
            let target = canonicalize_container_target(&triple.container_target);
            let (rights, intent_ro) = match triple.mode {
                MountMode::Ro => (Rights::Read, true),
                MountMode::Rw => (Rights::Full, false),
            };
            grants.insert(
                target.clone(),
                EffectiveGrant {
                    target,
                    rights,
                    claimants: vec![(Provenance::Profile, rights)],
                    binding_intent_ro: intent_ro,
                },
            );
        }
        let contributed: std::collections::HashSet<String> = placements
            .iter()
            .map(|placement| canonicalize_container_target(&placement.container_target))
            .collect();
        let mut seen: Vec<(String, Provenance, Rights)> = Vec::new();
        for permission in permissions {
            let target = canonicalize_container_target(&permission.target);
            let key = (
                target.clone(),
                permission.provenance.clone(),
                permission.rights,
            );
            if seen.contains(&key) {
                return Err(CistellaError::Contract(format!(
                    "duplicate permission for {target}"
                )));
            }
            seen.push(key);
            if !contributed.contains(&target)
                && !grants.contains_key(&target)
                && !baselines.contains(&target)
            {
                return Err(CistellaError::Contract(format!(
                    "permission for unknown grant target {target}"
                )));
            }
            let rights = match permission.rights {
                Rights::Write => Rights::Full,
                claimed => claimed,
            };
            grants
                .entry(target.clone())
                .and_modify(|grant| {
                    grant.rights = grant.rights.join(rights);
                    grant
                        .claimants
                        .push((permission.provenance.clone(), permission.rights));
                    grant.binding_intent_ro =
                        grant.binding_intent_ro || matches!(permission.rights, Rights::Read);
                })
                .or_insert(EffectiveGrant {
                    target,
                    rights,
                    claimants: vec![(permission.provenance.clone(), permission.rights)],
                    binding_intent_ro: matches!(permission.rights, Rights::Read),
                });
        }
        for target in &contributed {
            if !grants.contains_key(target) {
                return Err(CistellaError::Contract(format!(
                    "contributed placement {target} has no matching permission"
                )));
            }
        }
        Ok(Self { grants })
    }

    /// Looks up the effective grant for a canonical target.
    #[must_use]
    pub fn get(&self, canonical_target: &str) -> Option<&EffectiveGrant> {
        self.grants.get(canonical_target)
    }

    /// Iterates canonical targets with their grants.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &EffectiveGrant)> {
        self.grants.iter()
    }

    /// Ingests additional driver triples (mode implies policy,
    /// same feed as assembly): staged-hook bindings join the map
    /// after merge with driver provenance.
    pub fn ingest_driver(&mut self, triples: &[MountTriple]) {
        for triple in triples {
            let target = canonicalize_container_target(&triple.container_target);
            let (rights, intent_ro) = match triple.mode {
                MountMode::Ro => (Rights::Read, true),
                MountMode::Rw => (Rights::Full, false),
            };
            self.grants.insert(
                target.clone(),
                EffectiveGrant {
                    target,
                    rights,
                    claimants: vec![(Provenance::Profile, rights)],
                    binding_intent_ro: intent_ro,
                },
            );
        }
    }

    /// Grants for exactly these canonical targets, sorted by
    /// target for deterministic plan order.
    #[must_use]
    pub fn select_sorted(&self, targets: &[String]) -> Vec<EffectiveGrant> {
        let mut out: Vec<EffectiveGrant> = targets
            .iter()
            .filter_map(|target| self.grants.get(target).cloned())
            .collect();
        out.sort_by(|a, b| a.target.cmp(&b.target));
        out
    }
}

/// Renders contributed placements to binding triples from joined
/// rights (`Read`→ro, else rw): the SOLE post-merge source of
/// contributed modes. Every binding-mechanics consumer downstream
/// reads these rendered modes; grant classification reads the map
/// directly. Render here, nowhere else.
#[must_use]
pub fn render_placements(
    placements: &[Placement],
    effective: &EffectivePermissions,
) -> Vec<MountTriple> {
    use crate::mount::canonicalize_container_target;
    placements
        .iter()
        .map(|placement| {
            let target = canonicalize_container_target(&placement.container_target);
            let mode = match effective.get(&target).map(|grant| grant.rights) {
                Some(Rights::Read) | None => MountMode::Ro,
                _ => MountMode::Rw,
            };
            MountTriple {
                host_source: placement.host_source.clone(),
                container_target: placement.container_target.clone(),
                mode,
            }
        })
        .collect()
}
