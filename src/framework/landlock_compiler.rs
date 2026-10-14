//! Landlock-owned post-merge rendering compiler.
//!
//! Grant computation, FULL-source accounting, baseline paths and
//! masks, the safe RO→RW revision, and retention-contradiction
//! refusal. Relocated from framework `prepare`/`mount` with
//! behavior pinned at the move: the framework calls this code,
//! it does not reimplement it. Framework-side validation
//! (topology gates, grammar, snapshot binding) stays outside.
//!
//! The compile entry takes a frozen snapshot and returns a bound
//! rendered plan: starting as an in-process module called by the
//! framework (a future cross-process render op reuses the same
//! snapshot/output contract).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{CistellaError, Result};
use crate::framework::contract::{EffectivePermissions, GuestHookRequest, Rights};
use crate::mount::{
    MountMode, MountTriple, Placement, canonicalize_container_target, canonicalize_host_source,
    under_full_route, under_full_source,
};

/// One decided Landlock grant: full rights or read-execute on a
/// guest target, in triple order (rendering dedups first-wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantKind {
    Full,
    Read,
}

/// Decided grants over the mount topology: ancestor
/// read-execute routes, subtree full-rights routes, and the
/// per-triple carveout/readability decisions in triple order.
/// Shared by argv composition and the RO-retention revision so
/// the two cannot disagree on which guest targets receive FULL.
pub(crate) struct GrantSet {
    pub(crate) ancestor_routes: Vec<String>,
    pub(crate) subtree_routes: Vec<String>,
    pub(crate) carveouts: Vec<(GrantKind, String)>,
}

/// Computes the decided grants (pure; refuses exactly as
/// composition does on hook count and untranslatable roots).
///
/// # Errors
///
/// Returns `CistellaError::Contract` on hook count, empty routes,
/// or untranslatable roots.
pub(crate) fn compute_grants(
    hooks: &[GuestHookRequest],
    placements: &[crate::mount::Placement],
    effective: &EffectivePermissions,
    ancestor_host: &std::path::Path,
    subtree_host: &std::path::Path,
) -> Result<GrantSet> {
    if hooks.len() != 1 {
        return Err(CistellaError::Contract(
            "hook launch supports exactly one hook".to_string(),
        ));
    }
    let ancestor_routes = crate::mount::guest_routes_for_host(placements, ancestor_host);
    if ancestor_routes.is_empty() {
        return Err(CistellaError::Contract(
            "hook ancestor untranslatable through the mount topology".to_string(),
        ));
    }
    let subtree_routes = crate::mount::guest_routes_for_host(placements, subtree_host);
    if subtree_routes.is_empty() {
        return Err(CistellaError::Contract(
            "hook subtree untranslatable through the mount topology".to_string(),
        ));
    }
    // Declared mounts, granted by declared mode (operator-owned
    // intent; Landlock enforces, never second-guesses). Only
    // proven non-directories skip: files, sockets, fifos, and
    // their kind cannot root a `path_beneath` rule, and the
    // wrapper opens every allow path O_DIRECTORY — feeding
    // one fails apply with ENOTDIR (proven live: a
    // credential-surface socket refused the whole session).
    // Unix-socket connect needs no grant on this fleet
    // (spike-proven: connect succeeds under R+X baseline
    // with the socket itself unruled), so skipping loses no
    // working shape. Not-yet-existing paths still grant by
    // mode (fail-closed: a wrong-kind materialization fails
    // loudly at apply, never silently unconfined).
    let mut carveouts = Vec::new();
    for placement in placements {
        // Proven non-directories skip (see above); missing
        // paths grant by rights (see above).
        let source = crate::mount::canonicalize_host_source(&placement.host_source);
        if source.exists() && !source.is_dir() {
            continue;
        }
        // Grant computation runs on canonical target spellings —
        // the same form rendering emits and routes derive in —
        // so a non-canonical declaration (`/x/../work/ro-data`,
        // duplicate slashes, dot segments) cannot dodge coverage
        // or mint a second rule spelling for one mount.
        // Rights come from the effective map (actual granted
        // rights), never from wire modes — the wire carries none.
        let target = crate::mount::canonicalize_container_target(&placement.container_target);
        let grant = effective.get(&target).ok_or_else(|| {
            CistellaError::Contract(format!("no effective grant for granted placement {target}"))
        })?;
        let covered = ancestor_routes
            .iter()
            .chain(subtree_routes.iter())
            .any(|route| target == *route || target.starts_with(&format!("{route}/")));
        match grant.rights {
            // Read-write carveouts: EVERY declared read-write
            // directory grants full rights on its guest target,
            // wherever it sits — under-ancestor grafts, the
            // read-write ancestor binding itself, outside mounts
            // bound inside ancestor routes (`/opt/state` at
            // `/src/state`) alike. Declarations
            // are authoritative intent: a validated RW triple the
            // ruleset left read-only would fail writes the
            // operator declared admissible.
            crate::framework::contract::Rights::Full => {
                carveouts.push((GrantKind::Full, target));
            }
            // Read-only readability: uncovered outside mounts,
            // plus under-ancestor mounts whose targets lie
            // outside every route (declared content must stay
            // readable; default-deny would brick it). Targets
            // under routes stay covered by their route's rule.
            crate::framework::contract::Rights::Read if !covered => {
                carveouts.push((GrantKind::Read, target));
            }
            _ => {}
        }
    }
    Ok(GrantSet {
        ancestor_routes,
        subtree_routes,
        carveouts,
    })
}

/// Fixed FULL baselines every hooked launch grants outside the
/// mount topology (system composition) plus guest `/tmp`:
/// `/dev` is container-private (no host aliases possible);
/// `/tmp` is admitted by construction under hook (operator
/// direction — whatever backs it), with baseline alias
/// accounting (see `mount_alias::baseline_tmp_alias_preflight`)
/// closing the host-source alias hole a blanket FULL would
/// otherwise open. Guest targets receiving full rights
/// unconditionally. Shared with the RO-retention revision so
/// FULL accounting is complete — retention and preflight must
/// see every FULL route, not just topology-derived ones. (`/`
/// stays excluded: it grants read-execute, which union-safely
/// denies writes.)
pub(crate) const FULL_BASELINE_ROUTES: &[&str] = &["/dev", "/tmp"];

/// Owner-provided known image-private baselines (the fixed FULL
/// set needing no placement). Production merge contexts fill
/// this explicitly; the constant lives with its owning
/// compiler, not smuggled into generic merge code.
#[must_use]
pub(crate) fn baseline_routes() -> Vec<String> {
    FULL_BASELINE_ROUTES
        .iter()
        .map(|route| route.to_string())
        .collect()
}

/// Guest targets receiving FULL Landlock rights: the fixed
/// baselines, the subtree routes, plus every read-write
/// carveout target. The revision consumes this set to retain
/// Podman read-only on RO mounts nested under FULL routes
/// (Landlock union cannot subtract, so the VFS binding carries
/// that enforcement). Pure; refuses exactly as composition
/// does.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on hook count, empty routes,
/// or untranslatable roots.
pub fn full_grant_routes(
    hooks: &[GuestHookRequest],
    placements: &[crate::mount::Placement],
    effective: &EffectivePermissions,
    ancestor_host: &std::path::Path,
    subtree_host: &std::path::Path,
) -> Result<Vec<String>> {
    let grants = compute_grants(hooks, placements, effective, ancestor_host, subtree_host)?;
    let mut full: Vec<String> = FULL_BASELINE_ROUTES
        .iter()
        .map(|route| route.to_string())
        .collect();
    full.extend(grants.subtree_routes);
    for (kind, target) in grants.carveouts {
        if kind == GrantKind::Full && !full.contains(&target) {
            full.push(target);
        }
    }
    Ok(full)
}

pub fn revise_ro_for_confinement(
    triples: &[MountTriple],
    full_routes: &[String],
    full_sources: &[std::path::PathBuf],
) -> Vec<MountTriple> {
    triples
        .iter()
        .map(|triple| {
            let directory =
                triple.mode == MountMode::Ro && std::path::Path::new(&triple.host_source).is_dir();
            if !directory {
                return triple.clone();
            }
            let source = canonicalize_host_source(&triple.host_source);
            if under_full_route(&triple.container_target, full_routes)
                || under_full_source(&source, full_sources)
            {
                triple.clone()
            } else {
                MountTriple {
                    mode: MountMode::Rw,
                    ..triple.clone()
                }
            }
        })
        .collect()
}

pub fn full_grant_sources(
    placements: &[Placement],
    effective: &crate::framework::contract::EffectivePermissions,
    subtree_host: &std::path::Path,
) -> Vec<PathBuf> {
    use crate::mount::canonicalize_container_target;
    let mut sources = vec![canonicalize_host_source(&subtree_host.to_string_lossy())];
    for placement in placements {
        // Actual granted rights from the map: a `Write` placement
        // joins FULL accounting exactly like `Full` (normalized
        // before every consumer), so no alias hole opens under a
        // non-FULL label.
        let target = canonicalize_container_target(&placement.container_target);
        let full = effective
            .get(&target)
            .is_some_and(|grant| grant.rights == Rights::Full);
        if full && std::path::Path::new(&placement.host_source).is_dir() {
            let source = canonicalize_host_source(&placement.host_source);
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
    }
    // Baseline-backed /tmp: the FULL rule lands on the backing
    // dentries whatever the declared rights, so the source joins
    // FULL accounting (retention keeps every RO route to it
    // read-only at the VFS layer; the alias preflight below
    // refuses the routes retention cannot cover).
    let backing = placements.iter().find_map(|placement| {
        if canonicalize_container_target(&placement.container_target) == "/tmp" {
            Some(canonicalize_host_source(&placement.host_source))
        } else {
            None
        }
    });
    if let Some(backing) = backing
        && backing.is_dir()
        && !sources.contains(&backing)
    {
        sources.push(backing);
    }
    sources
}

/// Refuses contradictory RO-under-FULL topologies pre-create
/// (tier-2 hardening): a directory RO mount retained read-only
/// — by FULL target coverage OR FULL-backed source — with a
/// declared read-write triple beneath its own target cannot
/// hold — keeping the RO binding read-only would brick the RW
/// descendant's materialization, and flipping it would expose
/// the RO source through the FULL grant. Such topologies
/// refuse with the conflicting pair named; the operator
/// re-declares (moves the RW graft out from under the RO dir).
/// Runs pre-create, so a refusal leaves no residue.
///
/// # Errors
///
/// Returns `CistellaError::Mount` on the first contradictory pair.
pub fn ro_confinement_preflight(
    triples: &[MountTriple],
    full_routes: &[String],
    full_sources: &[std::path::PathBuf],
) -> Result<()> {
    for ro in triples {
        // Binding truth, not granted rights: assembled modes are
        // intent-aware by construction (driver modes real,
        // contributed modes rendered from intent), so a
        // read-only BINDING with a read-write descendant beneath
        // refuses regardless of what the union grants. Grant
        // aliasing is the graft preflight's job, not this one's.
        if ro.mode != MountMode::Ro || !std::path::Path::new(&ro.host_source).is_dir() {
            continue;
        }
        let retained = under_full_route(&ro.container_target, full_routes)
            || under_full_source(&canonicalize_host_source(&ro.host_source), full_sources);
        if !retained {
            continue;
        }
        // Descendant comparison runs on canonical spellings
        // (same rationale as retention): alias spellings of the
        // same target still collide.
        let ro_canon = canonicalize_container_target(&ro.container_target);
        for rw in triples {
            let rw_canon = canonicalize_container_target(&rw.container_target);
            if rw.mode == MountMode::Rw
                && rw_canon != ro_canon
                && rw_canon.starts_with(&format!("{ro_canon}/"))
            {
                return Err(CistellaError::Mount(format!(
                    "read-only {} under a full grant holds a read-write descendant {}: move the graft out",
                    ro.container_target, rw.container_target
                )));
            }
        }
    }
    Ok(())
}

/// Resolves the owning compiler for a claiming hook by its
/// artifact identity: the shipped landlock supervisor artifact
/// selects this compiler. Anything else refuses unsupported —
/// a placement-less isolator or foreign supervisor declares
/// its own renderer (or explicit refusal) rather than
/// silently borrowing this one. Adding a backend is a new
/// match arm plus its compiler module, never a framework
/// special case.
///
/// # Errors
///
/// Returns `CistellaError::Contract` when no compiler is
/// registered for the artifact.
pub(crate) fn compiler_owner_for(hook: &GuestHookRequest) -> Result<&'static str> {
    use crate::framework::registry::{SHIPPED_REGISTRY_ID, WRAPPER_FILE_NAME};
    if hook.artifact.source.registry == SHIPPED_REGISTRY_ID
        && hook.artifact.source.path == WRAPPER_FILE_NAME
    {
        Ok("landlock")
    } else {
        Err(CistellaError::Contract(format!(
            "no compiler registered for hook artifact {}:{}",
            hook.artifact.source.registry, hook.artifact.source.path
        )))
    }
}

/// Frozen admitted snapshot: the compiler's complete input.
///
/// Placements, assembled triples, the full effective map, roots,
/// claiming hook digest, and a fingerprint over the canonical
/// representation of ALL of it. The fingerprint binds output to
/// input: a rendering computed from any other snapshot refuses
/// at application. Snapshots with the same targets backed by
/// different host sources fingerprint differently; provenance
/// labels and non-placement grants are covered too.
#[derive(Debug, Clone)]
pub struct CompileSnapshot {
    /// Canonical placements under compilation.
    pub placements: Vec<Placement>,
    /// Assembled binding triples (driver sugar plus rendered
    /// contributions plus staged hooks).
    pub triples: Vec<MountTriple>,
    /// Complete effective map (driver, extension, and baseline
    /// matched grants alike).
    pub effective: EffectivePermissions,
    /// Confinement ancestor host path.
    pub ancestor_host: PathBuf,
    /// Confinement subtree host path.
    pub subtree_host: PathBuf,
    /// SHA-256 of the claiming hook artifact (binds output to
    /// the enforcement consumer — no CompilerA rendering paired
    /// with a SupervisorB policy).
    pub hook_digest: String,
    /// Owning compiler that must render this snapshot
    /// (resolved from the claiming hook, echoed by output).
    pub compiler_owner: String,
    /// SHA-256 over the canonical snapshot contents.
    pub fingerprint: String,
}

/// Rendered plan bound to its snapshot: revised bindings plus
/// the decided sets, echoing fingerprint and hook digest for
/// framework verification before create.
#[derive(Debug, Clone)]
pub struct CompiledTopology {
    /// Revised binding triples for the isolator.
    pub revised: Vec<MountTriple>,
    /// Decided FULL routes (same set the alias gates consume).
    pub routes: Vec<String>,
    /// Decided FULL-backed sources.
    pub sources: Vec<PathBuf>,
    /// Echo of the input fingerprint (must match to apply).
    pub fingerprint: String,
    /// Echo of the claiming hook digest (must match to apply).
    pub hook_digest: String,
    /// Echo of the owning compiler (must match to apply).
    pub compiler_owner: String,
}

/// Canonical snapshot encoding for the fingerprint: sorted
/// placements (target and source), sorted full-map grants
/// (target, rights, intent, claimant labels), roots, hook.
fn fingerprint_snapshot(
    placements: &[Placement],
    triples: &[MountTriple],
    effective: &EffectivePermissions,
    ancestor_host: &Path,
    subtree_host: &Path,
    hook_digest: &str,
    compiler_owner: &str,
) -> String {
    // Structured JSON encoding, sorted for determinism:
    // field boundaries are structural, never delimiter
    // inference (a `|` inside a path cannot collide fields).
    use serde::Serialize;
    #[derive(Serialize)]
    struct BoundPlacement<'a> {
        target: &'a str,
        source: &'a str,
    }
    #[derive(Serialize)]
    struct BoundTriple<'a> {
        target: &'a str,
        source: &'a str,
        mode: &'a str,
    }
    #[derive(Serialize)]
    struct BoundGrant<'a> {
        target: &'a str,
        rights: &'a str,
        intent_ro: bool,
        claimants: Vec<String>,
    }
    #[derive(Serialize)]
    struct SnapshotRecord<'a> {
        placements: Vec<BoundPlacement<'a>>,
        triples: Vec<BoundTriple<'a>>,
        grants: Vec<BoundGrant<'a>>,
        ancestor: &'a str,
        subtree: &'a str,
        hook_digest: &'a str,
        compiler_owner: &'a str,
    }
    fn rights_name(rights: Rights) -> &'static str {
        match rights {
            Rights::Read => "read",
            Rights::Write => "write",
            Rights::Full => "full",
        }
    }
    let mut placement_records: Vec<BoundPlacement<'_>> = placements
        .iter()
        .map(|placement| BoundPlacement {
            target: &placement.container_target,
            source: &placement.host_source,
        })
        .collect();
    placement_records.sort_by(|a, b| (a.target, a.source).cmp(&(b.target, b.source)));
    let mut triple_records: Vec<BoundTriple<'_>> = triples
        .iter()
        .map(|triple| BoundTriple {
            target: &triple.container_target,
            source: &triple.host_source,
            mode: triple.mode.as_flag(),
        })
        .collect();
    triple_records.sort_by(|a, b| (a.target, a.source, a.mode).cmp(&(b.target, b.source, b.mode)));
    let mut grant_records: Vec<BoundGrant<'_>> = effective
        .iter()
        .map(|(target, grant)| {
            let mut claimants: Vec<String> = grant
                .claimants
                .iter()
                .map(|(provenance, rights)| format!("{provenance:?}:{rights:?}"))
                .collect();
            claimants.sort();
            BoundGrant {
                target,
                rights: rights_name(grant.rights),
                intent_ro: grant.binding_intent_ro,
                claimants,
            }
        })
        .collect();
    grant_records.sort_by(|a, b| a.target.cmp(b.target));
    let record = SnapshotRecord {
        placements: placement_records,
        triples: triple_records,
        grants: grant_records,
        ancestor: &ancestor_host.to_string_lossy(),
        subtree: &subtree_host.to_string_lossy(),
        hook_digest,
        compiler_owner,
    };
    let bytes = serde_json::to_vec(&record).expect("snapshot record serializes");
    hex::encode(Sha256::digest(&bytes))
}

/// Recomputes a snapshot's fingerprint from its CURRENT
/// contents: snapshot-field mutation cannot reuse a stale
/// fingerprint, because verification recomputes rather than
/// trusting the stored string.
pub(crate) fn recompute_fingerprint(snapshot: &CompileSnapshot) -> String {
    fingerprint_snapshot(
        &snapshot.placements,
        &snapshot.triples,
        &snapshot.effective,
        &snapshot.ancestor_host,
        &snapshot.subtree_host,
        &snapshot.hook_digest,
        &snapshot.compiler_owner,
    )
}

/// Freezes the admitted snapshot: placements, assembled
/// triples, the full effective map, roots, and the claiming
/// hook digest, fingerprinted over the canonical
/// representation.
///
/// Refuses typed on duplicate canonical placement IDs and on a
/// placement with no effective grant (both unreachable past
/// merge validation — the snapshot is self-validating so a
/// future cross-process caller inherits the same gates).
///
/// # Errors
///
/// Returns `CistellaError::Contract` on duplicate IDs or a grant
/// gap.
pub fn freeze_snapshot(
    placements: &[Placement],
    triples: &[MountTriple],
    effective: &EffectivePermissions,
    ancestor_host: &Path,
    subtree_host: &Path,
    hook: &GuestHookRequest,
) -> Result<CompileSnapshot> {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for placement in placements {
        let target = canonicalize_container_target(&placement.container_target);
        if !seen.insert(target.clone()) {
            return Err(CistellaError::Contract(format!(
                "compile snapshot duplicate placement target {target}"
            )));
        }
        if effective.get(&target).is_none() {
            return Err(CistellaError::Contract(format!(
                "compile snapshot placement {target} has no effective grant"
            )));
        }
    }
    let owner = compiler_owner_for(hook)?;
    let fingerprint = fingerprint_snapshot(
        placements,
        triples,
        effective,
        ancestor_host,
        subtree_host,
        &hook.artifact.sha256,
        owner,
    );
    Ok(CompileSnapshot {
        placements: placements.to_vec(),
        triples: triples.to_vec(),
        effective: effective.clone(),
        ancestor_host: ancestor_host.to_path_buf(),
        subtree_host: subtree_host.to_path_buf(),
        hook_digest: hook.artifact.sha256.clone(),
        compiler_owner: owner.to_string(),
        fingerprint,
    })
}

/// Compiles the frozen snapshot ALONE to rendered bindings:
/// decided FULL routes and sources plus the safe RO→RW revision
/// over the snapshot's assembled triples. Binds the input
/// fingerprint and hook digest into the output for framework
/// verification.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on grant derivation.
pub fn compile_topology(
    snapshot: &CompileSnapshot,
    hook: &GuestHookRequest,
) -> Result<CompiledTopology> {
    // Single-hook admission upstream guarantees exactly one
    // claimant here; route derivation reads placements and the
    // map, never hook content — the hook only satisfies the
    // singleton shape gate and binds the output digest.
    let routes = full_grant_routes(
        std::slice::from_ref(hook),
        &snapshot.placements,
        &snapshot.effective,
        &snapshot.ancestor_host,
        &snapshot.subtree_host,
    )?;
    let sources = full_grant_sources(
        &snapshot.placements,
        &snapshot.effective,
        &snapshot.subtree_host,
    );
    let revised = revise_ro_for_confinement(&snapshot.triples, &routes, &sources);
    Ok(CompiledTopology {
        revised,
        routes,
        sources,
        fingerprint: snapshot.fingerprint.clone(),
        hook_digest: snapshot.hook_digest.clone(),
        compiler_owner: snapshot.compiler_owner.clone(),
    })
}
