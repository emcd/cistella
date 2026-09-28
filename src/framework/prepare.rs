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
use crate::framework::registry::{
    MIN_LANDLOCK_ABI, REQUIRED_HANDLED_FS, STAGED_WRAPPER_GUEST_PATH,
};

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
    // Extension read-write mounts are never admitted (tier-2
    // hardening): merged mounts are extension-provided by
    // construction — profile/CLI/session triples ride the occupied
    // baseline, never the plan — and no Mounts policy admission
    // exists, so an RW triple here would compose into a FULL
    // Landlock carveout on untrusted say-so. Read-only
    // contributions still merge (vectors pin the bus-socket
    // shape): they cannot grant writes through compose, and the
    // RO-under-FULL retention keeps their Podman binding
    // read-only. Remove this refusal only with explicit policy
    // admission for extension mounts.
    refuse_extension_rw_mounts(&merged.mounts)?;
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
    let plan = match (outcome, shutdown) {
        (Ok(plan), Ok(())) => plan,
        (_, Err(residue)) => return Err(residue),
        (Err(error), Ok(())) => return Err(error),
    };
    // Artifact-executable binding (3.2 handoff invariant, enforced
    // from 3.1): the hook executable must be exactly the staged
    // wrapper path — a correctly digest-pinned artifact with
    // `argv_prefix[0]` naming `/bin/sh` (or any other absolute
    // executable) would bypass confinement at composition. The
    // prepare payload carries no session context, so session-blind
    // extension args are never legitimate either: the prefix is the
    // singleton staged path, and the framework composes all wrapper
    // arguments at launch (task 3.2). Raw argv crosses verbatim by
    // exec (no shell), so no byte-class filtering applies — the gate
    // is structural identity, not content.
    for hook in &plan.merged.guest_hooks {
        check_hook_executable(hook)?;
    }
    Ok(plan)
}

/// Checks one merged hook names exactly the staged wrapper
/// executable (singleton prefix).
///
/// # Errors
///
/// Returns `CistellaError::Contract` on any other executable or
/// prefix length.
pub fn check_hook_executable(hook: &GuestHookRequest) -> Result<()> {
    if hook.argv_prefix.len() != 1 || hook.argv_prefix[0] != STAGED_WRAPPER_GUEST_PATH {
        return Err(CistellaError::Contract(
            "guest hook argv must name exactly the staged wrapper path".to_string(),
        ));
    }
    Ok(())
}

/// Refuses extension read-write mount contributions: merged
/// mounts are extension-provided by construction, and no Mounts
/// policy admission exists, so an RW triple would compose into a
/// FULL Landlock carveout on untrusted say-so. Pure over the
/// merged mount set; diagnostics name the contribution type,
/// never paths.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on the first RW triple.
pub fn refuse_extension_rw_mounts(mounts: &[MountTriple]) -> Result<()> {
    if mounts.iter().any(|t| t.mode == MountMode::Rw) {
        return Err(CistellaError::Contract(
            "extension read-write mount contributions are not admitted: no Mounts policy admission exists"
                .to_string(),
        ));
    }
    Ok(())
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

/// Exact `--probe` success shape: kernel ABI plus handled mask.
/// Derived `Deserialize` rejects duplicate fields and (with
/// `deny_unknown_fields`) any extra keys — the report is an exact
/// contract, not a loose map.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeOk {
    abi: u64,
    handled_fs_mask: u64,
}

/// Exact `--probe` refusal shape: wrapper-observed unsupported reason.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeUnsupported {
    unsupported: String,
}

/// Exact probe report: success or refusal, nothing else.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ProbeReport {
    Ok(ProbeOk),
    Unsupported(ProbeUnsupported),
}

/// Exact attestation shape: applied with ABI and handled mask,
/// or refused with a reason. Derived `Deserialize` rejects
/// duplicate fields and extra keys; the cross-field check below
/// enforces the tagged pairing (abi+mask xor error). The mask
/// repeats so the gate re-verifies the matrix at this second
/// trust moment (probe ran earlier as a separate `podman exec`
/// on stdout, not the same pipe, so no same-pipe provenance is
/// claimed). Independent absolute floors (ABI minimum, mask
/// coverage) are the contract; exact probe/apply match is not
/// required (the kernel ABI is fixed per boot, and wrapper
/// construction enforces real adequacy).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Attestation {
    applied: bool,
    abi: Option<u64>,
    handled_fs_mask: Option<u64>,
    error: Option<String>,
}

/// Checks a wrapper `--probe` report: kernel ABI plus handled
/// mask. Shortfall refuses typed (fail pre-execute); the mask is
/// never narrowed to fit the kernel.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shape violation,
/// wrapper-reported unsupported, low ABI, or rights shortfall.
pub fn parse_probe_report(stdout: &[u8]) -> Result<()> {
    let report: ProbeReport = serde_json::from_slice(stdout)
        .map_err(|_| CistellaError::Contract("bad probe report: shape".to_string()))?;
    match report {
        ProbeReport::Unsupported(unsupported) => Err(CistellaError::Contract(format!(
            "landlock unsupported: {}",
            unsupported.unsupported
        ))),
        ProbeReport::Ok(ok) => {
            if ok.abi < u64::from(MIN_LANDLOCK_ABI) {
                return Err(CistellaError::Contract(format!(
                    "landlock unsupported: kernel ABI {} below minimum {}",
                    ok.abi, MIN_LANDLOCK_ABI
                )));
            }
            if ok.handled_fs_mask & REQUIRED_HANDLED_FS != REQUIRED_HANDLED_FS {
                return Err(CistellaError::Contract(
                    "landlock unsupported: kernel rights shortfall".to_string(),
                ));
            }
            Ok(())
        }
    }
}

/// Parses one wrapper attestation line: returns `(ABI, mask)` on
/// an exact applied shape whose matrix meets the floor
/// (ABI≥minimum, mask covering the required rights). `applied:false`
/// or any other shape refuses typed — session start never proceeds
/// past a failed apply or a short matrix, even attested.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shape violation (including
/// duplicate or unknown fields), a negative attestation, a low
/// ABI, or a rights shortfall.
pub fn parse_attestation_line(line: &str) -> Result<(u64, u64)> {
    let attestation: Attestation = serde_json::from_str(line)
        .map_err(|_| CistellaError::Contract("bad attestation: shape".to_string()))?;
    match (
        attestation.applied,
        attestation.abi,
        attestation.handled_fs_mask,
        attestation.error,
    ) {
        (true, Some(abi), Some(mask), None) => {
            if abi < u64::from(MIN_LANDLOCK_ABI) {
                return Err(CistellaError::Contract(format!(
                    "applied attestation ABI {abi} below minimum {}",
                    MIN_LANDLOCK_ABI
                )));
            }
            if mask & REQUIRED_HANDLED_FS != REQUIRED_HANDLED_FS {
                return Err(CistellaError::Contract(
                    "applied attestation rights shortfall".to_string(),
                ));
            }
            Ok((abi, mask))
        }
        (false, _, _, Some(error)) => Err(CistellaError::Contract(format!(
            "wrapper reported apply failure: {error}"
        ))),
        _ => Err(CistellaError::Contract(
            "bad attestation: shape".to_string(),
        )),
    }
}

/// Reads one attestation line from a diagnostics read-end under an
/// absolute deadline: polls for readability, accumulates to the
/// first newline (64 KiB cap), and returns the line plus any
/// already-buffered trailing bytes (they belong to the drain
/// phase, never discarded). EOF before the newline refuses typed
/// (the wrapper died before attesting); timeout refuses typed.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on EOF, timeout, overlong
/// output, or wait failure, and `CistellaError::Runtime` on read
/// failure.
pub fn read_attestation_line(
    read: std::os::fd::BorrowedFd<'_>,
    timeout: std::time::Duration,
) -> Result<(String, Vec<u8>)> {
    use nix::poll::{PollFd, PollFlags, poll};
    use std::os::fd::{AsFd, AsRawFd};
    let deadline = std::time::Instant::now() + timeout;
    let mut buffered = Vec::new();
    loop {
        if let Some(position) = buffered.iter().position(|&byte| byte == b'\n') {
            let line = String::from_utf8_lossy(&buffered[..position]).into_owned();
            let rest = buffered[position + 1..].to_vec();
            return Ok((line, rest));
        }
        if buffered.len() > 64 * 1024 {
            return Err(CistellaError::Contract(
                "hook diagnostics overlong before attestation".to_string(),
            ));
        }
        let wait = nix::poll::PollTimeout::try_from(
            deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(std::time::Duration::ZERO),
        )
        .map_err(|_| CistellaError::Runtime("attestation wait out of range".to_string()))?;
        let mut pollfds = [PollFd::new(read.as_fd(), PollFlags::POLLIN)];
        let ready = poll(&mut pollfds, wait)
            .map_err(|e| CistellaError::Contract(format!("attestation wait failed: {e}")))?;
        if ready == 0 {
            return Err(CistellaError::Contract(
                "hook attestation timed out".to_string(),
            ));
        }
        let mut chunk = [0u8; 8192];
        let raw = read.as_fd().as_raw_fd();
        // SAFETY: borrowed read-end, transient buffer, return
        // checked below; no ownership transfer.
        let count = unsafe { nix::libc::read(raw, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count < 0 {
            return Err(CistellaError::Runtime(
                "diagnostics read failed".to_string(),
            ));
        }
        if count == 0 {
            return Err(CistellaError::Contract(
                "hook diagnostics closed before attestation".to_string(),
            ));
        }
        buffered.extend_from_slice(&chunk[..count as usize]);
    }
}

/// Derives the confinement roots: the `~/src` ancestor plus the
/// canonical session subtree. Sessions outside the operator's src
/// tree refuse fail-closed (no translatable ancestor exists).
///
/// # Errors
///
/// Returns `CistellaError::Contract` when the session directory
/// leaves the confinement root.
pub fn confinement_roots(
    home: &std::path::Path,
    session_dir: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let ancestor = home.join("src");
    let subtree = std::path::PathBuf::from(session_dir);
    if !subtree.starts_with(&ancestor) {
        return Err(CistellaError::Contract(
            "session outside ~/src confinement root".to_string(),
        ));
    }
    Ok((ancestor, subtree))
}

/// Composes one hooked launch argv: the admitted hook's staged
/// executable, framework-owned wrapper args, the `--` separator,
/// then the harness argv verbatim. Exactly one hook is supported
/// (single wrapper chain); untranslatable roots refuse fail-closed.
/// Pure: all inputs explicit, pinned fast without podman.
///
/// Wrapper args (operator-decided policy, mechanical composition):
/// - `--allow-ro=/`: system baseline for the loader, interpreter,
///   and libc (read plus execute, never write). Without it no
///   ordinary harness starts; Landlock is default-deny. The rule
///   lands on the container rootfs (the image mount nothing else
///   covers), not on any host path.
/// - `--allow-rw=/dev`: device essentials (`null`, `zero`,
///   `urandom`, `shm`, `pts`) need writes a read-only rule
///   denies. Scoped to the container-private devtmpfs, where
///   kernel capabilities neuter real device creation; file-level
///   granularity is unexpressible (`path_beneath` needs directory
///   roots), so the whole dir grants full. The one debatable
///   entry: dogfood demotes it only on evidence.
/// - ancestor guest routes as read-execute; subtree guest routes
///   as full rights (the union exception).
/// - RW carveouts: EVERY declared read-write directory grants
///   full rights on its guest target, wherever it sits — the
///   project subtree, the read-write ancestor binding itself,
///   per-project grafts under shared read-only trees, outside
///   host sources bound inside ancestor routes (`/opt/state`
///   at `/src/state`), uncovered scratch alike. Declarations
///   are authoritative intent: a validated RW triple left
///   read-only would fail writes the operator declared
///   admissible.
/// - RO readability: every read-only triple whose target lies
///   outside all routes grants read-execute (declared content
///   must stay readable; default-deny would brick it). Targets
///   under RO ancestor routes stay covered by that rule, which
///   denies writes at the Landlock layer; targets under FULL
///   routes carry no subtracted rule (union semantics forbid
///   it) and rely on the retained Podman read-only binding
///   instead (see the revision). Profile declarations are
///   authoritative intent (operator/seat-owned); Landlock
///   enforces them, it does not second-guess them. Proven files
///   skip (sockets cannot root a `path_beneath` rule);
///   not-yet-existing paths grant by mode (a wrong-kind
///   materialization fails loudly at apply).
/// - `/tmp`, `$HOME` (container-private tmpfs), and `/dev/null`
///   writes stay denied (outlets: `/tmp/scratch`): dogfood
///   promotes only on evidence.
///   Entries deduplicate (first occurrence wins); harness argv
///   appends verbatim after `--`.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on hook count, empty routes,
/// or untranslatable roots.
/// One decided Landlock grant: full rights or read-execute on a
/// guest target, in triple order (rendering dedups first-wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrantKind {
    Full,
    Read,
}

/// Decided grants over the mount topology: ancestor
/// read-execute routes, subtree full-rights routes, and the
/// per-triple carveout/readability decisions in triple order.
/// Shared by argv composition and the RO-retention revision so
/// the two cannot disagree on which guest targets receive FULL.
struct GrantSet {
    ancestor_routes: Vec<String>,
    subtree_routes: Vec<String>,
    carveouts: Vec<(GrantKind, String)>,
}

/// Computes the decided grants (pure; refuses exactly as
/// composition does on hook count and untranslatable roots).
///
/// # Errors
///
/// Returns `CistellaError::Contract` on hook count, empty routes,
/// or untranslatable roots.
fn compute_grants(
    hooks: &[GuestHookRequest],
    triples: &[MountTriple],
    ancestor_host: &std::path::Path,
    subtree_host: &std::path::Path,
) -> Result<GrantSet> {
    if hooks.len() != 1 {
        return Err(CistellaError::Contract(
            "hook launch supports exactly one hook".to_string(),
        ));
    }
    let ancestor_routes = crate::mount::guest_routes_for_host(triples, ancestor_host);
    if ancestor_routes.is_empty() {
        return Err(CistellaError::Contract(
            "hook ancestor untranslatable through the mount topology".to_string(),
        ));
    }
    let subtree_routes = crate::mount::guest_routes_for_host(triples, subtree_host);
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
    for triple in triples {
        // Proven non-directories skip (see above); missing
        // paths grant by mode (see above).
        let source = crate::mount::canonicalize_host_source(&triple.host_source);
        if source.exists() && !source.is_dir() {
            continue;
        }
        // Grant computation runs on canonical target spellings —
        // the same form rendering emits and routes derive in —
        // so a non-canonical declaration (`/x/../work/ro-data`,
        // duplicate slashes, dot segments) cannot dodge coverage
        // or mint a second rule spelling for one mount.
        let target = crate::mount::canonicalize_container_target(&triple.container_target);
        let covered = ancestor_routes
            .iter()
            .chain(subtree_routes.iter())
            .any(|route| target == *route || target.starts_with(&format!("{route}/")));
        match triple.mode {
            // Read-write carveouts: EVERY declared read-write
            // directory grants full rights on its guest target,
            // wherever it sits — under-ancestor grafts, the
            // read-write ancestor binding itself, outside mounts
            // bound inside ancestor routes (`/opt/state` at
            // `/src/state`), uncovered scratch alike. Declarations
            // are authoritative intent: a validated RW triple the
            // ruleset left read-only would fail writes the
            // operator declared admissible.
            MountMode::Rw => {
                carveouts.push((GrantKind::Full, target));
            }
            // Read-only readability: uncovered outside mounts,
            // plus under-ancestor mounts whose targets lie
            // outside every route (declared content must stay
            // readable; default-deny would brick it). Targets
            // under routes stay covered by their route's rule.
            MountMode::Ro if !covered => {
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
/// mount topology (system composition): guest targets receiving
/// full rights unconditionally. Shared with the RO-retention
/// revision so FULL accounting is complete — retention and
/// preflight must see every FULL route, not just
/// topology-derived ones. (`/` stays excluded: it grants
/// read-execute, which union-safely denies writes.)
const FULL_BASELINE_ROUTES: &[&str] = &["/dev"];

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
    triples: &[MountTriple],
    ancestor_host: &std::path::Path,
    subtree_host: &std::path::Path,
) -> Result<Vec<String>> {
    let grants = compute_grants(hooks, triples, ancestor_host, subtree_host)?;
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

pub fn compose_hook_argv(
    hooks: &[GuestHookRequest],
    triples: &[MountTriple],
    ancestor_host: &std::path::Path,
    subtree_host: &std::path::Path,
    harness_argv: &[String],
) -> Result<Vec<String>> {
    let grants = compute_grants(hooks, triples, ancestor_host, subtree_host)?;
    let hook = &hooks[0];
    let mut argv = Vec::new();
    argv.extend(hook.argv_prefix.iter().cloned());
    // System baseline first (fixed position, deterministic):
    // read-execute on the container rootfs, full rights on the
    // fixed FULL baselines (rendered from the shared set the
    // revision accounts, so composition and retention agree).
    argv.push("--allow-ro=/".to_string());
    for baseline in FULL_BASELINE_ROUTES {
        argv.push(format!("--allow-rw={baseline}"));
    }
    // Exact-duplicate flags collapse (first occurrence wins);
    // nested overlaps stay (union semantics need both the
    // ancestor read-execute and the carveout full rights).
    let mut push_unique = |flag: String| {
        if !argv.contains(&flag) {
            argv.push(flag);
        }
    };
    for route in &grants.ancestor_routes {
        push_unique(format!("--allow-ro={route}"));
    }
    for route in &grants.subtree_routes {
        push_unique(format!("--allow-rw={route}"));
    }
    for (kind, target) in grants.carveouts {
        match kind {
            GrantKind::Full => push_unique(format!("--allow-rw={target}")),
            GrantKind::Read => push_unique(format!("--allow-ro={target}")),
        }
    }
    argv.push("--".to_string());
    argv.extend(harness_argv.iter().cloned());
    Ok(argv)
}

/// Exact transition shape: the supervisor's exec-boundary
/// report. Derived `Deserialize` rejects duplicate fields and
/// extra keys; the cross-field check below enforces the tagged
/// pairing (error xor clean).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Transitioned {
    transitioned: bool,
    error: Option<String>,
}

/// Checks one post-attestation diagnostics line: `Ok(Some)` only
/// for the exact transition-failure shape, `Ok(None)` for the
/// exact clean-transition shape. There is no admissible third
/// shape (the supervisor emits exactly one transition line):
/// anything else refuses typed, so a malformed stream can never
/// classify a launch successful.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on any line that is not
/// exactly one of the two transition shapes.
pub fn check_transition_line(line: &str) -> Result<Option<String>> {
    let transitioned: Transitioned = serde_json::from_str(line)
        .map_err(|_| CistellaError::Contract("malformed diagnostics trailing".to_string()))?;
    match (transitioned.transitioned, transitioned.error) {
        (true, None) => Ok(None),
        (false, Some(error)) => Ok(Some(error)),
        _ => Err(CistellaError::Contract(
            "malformed diagnostics trailing".to_string(),
        )),
    }
}
