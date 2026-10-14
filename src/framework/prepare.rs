//! Prepare transaction over the protocol exchange.
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
//! Production `conduct` drives this transaction for every session;
//! the in-process path remains the conformance reference.

use std::collections::HashSet;
use std::io::Read;
use std::os::fd::AsFd;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{CistellaError, Result};
pub use crate::framework::contract::EvaluatedPlan;
use crate::framework::contract::{
    Deadlines, EffectivePermissions, EnvContribution, GuestHookRequest, MergeContext,
    MountContribution, MountTriple, PermissionContribution, Placement, PolicyClaim, PreparePlan,
    Provenance, Rights, merge_transacted,
};
use crate::framework::credentials::{AdmittedCredential, CredentialHandle, admit_all};
use crate::framework::guest::{host_external, host_registered};
use crate::framework::landlock_compiler::GrantKind;
use crate::framework::policy::{PolicySet, acceptance_set};
use crate::framework::protocol::Exchange;
use crate::framework::registry::{
    MIN_LANDLOCK_ABI, REQUIRED_HANDLED_FS, STAGED_WRAPPER_GUEST_PATH,
};
use crate::guest_source::ResolvedGuest;

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

/// Wire form of one mount contribution: placement only, no mode.
///
/// Removing `mode` (with `deny_unknown_fields` retained) is the
/// epoch-2 wire break: mode-bearing responses refuse on shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MountWire {
    /// Host path.
    host_source: String,
    /// Container path.
    container_target: String,
}

/// Wire form of one permission contribution.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionWire {
    /// Guest-visible target path.
    target: String,
    /// `read`, `write`, or `full`.
    rights: String,
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
    /// Permission contributions (default empty).
    #[serde(default)]
    permissions: Vec<PermissionWire>,
    /// Policy claims scoped to this transaction (default empty).
    #[serde(default)]
    policy_claims: Vec<PolicyClaim>,
    /// Guest-hook requests (default empty).
    #[serde(default)]
    guest_hooks: Vec<GuestHookRequest>,
    /// Cleanup declarations: absolute host paths the extension
    /// minted and owns (default empty).
    #[serde(default)]
    cleanup: Vec<String>,
    /// Credential handles, variants only, never values (default empty).
    #[serde(default)]
    credentials: Vec<CredentialHandle>,
}

/// Runs one prepare transaction against a guest exchange.
///
/// Sends `prepare` (carrying the opaque session id — extensions
/// MAY mint per-session host state keyed by it, MUST treat it as
/// opaque), parses the single response with unknown-field
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
/// Eight parameters mirror the transaction inputs one-to-one
/// (exchange, identity, capabilities, context, policy,
/// acceptances, budget, session); bundling would only move them.
#[allow(clippy::too_many_arguments)]
pub fn run_prepare<R: Read + AsFd, W: std::io::Write + AsFd>(
    exchange: &mut Exchange<R, W>,
    guest_id: &str,
    capabilities: &[String],
    context: &MergeContext,
    policy: &PolicySet,
    acceptances: &HashSet<String>,
    timeout: Duration,
    session_id: &str,
) -> Result<EvaluatedPlan> {
    let (plan, credentials) = transact_prepare(exchange, guest_id, timeout, session_id)?;
    // Single-set-single-request admission (single-guest path):
    // one guest returning two hook requests refuses here; the
    // multi-claimant half lives in the N-guest runner. The merge
    // below would order them, never admit them.
    if plan.guest_hooks.len() > 1 {
        return Err(CistellaError::Contract(format!(
            "extension guest {guest_id} returned {} hook requests: at most one",
            plan.guest_hooks.len()
        )));
    }
    merge_transacted(
        plan,
        credentials,
        guest_id,
        capabilities,
        context,
        policy,
        acceptances,
    )
}

/// Requests one prepare transaction and builds the typed plan:
/// sends the request (carrying the opaque session id — extensions
/// MAY mint per-session host state keyed by it, MUST treat it as
/// opaque; old decoders tolerate the additive field), parses the
/// single response with unknown-field refusal, and admits
/// credential handles by shape. Merging, claims, and evaluation
/// belong to [`merge_transacted`] so pair composition can
/// concatenate raw plans and merge once centrally.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on transport, correlation, or
/// shape failure, and `CistellaError::Contract` on credential
/// admission refusal.
pub(crate) fn transact_prepare<R: Read + AsFd, W: std::io::Write + AsFd>(
    exchange: &mut Exchange<R, W>,
    guest_id: &str,
    timeout: Duration,
    session_id: &str,
) -> Result<(PreparePlan, Vec<AdmittedCredential>)> {
    // The opaque session id rides the request (additive field):
    // extensions MAY mint per-session host state keyed by it and
    // MUST treat it as opaque. Old decoders tolerate the field
    // (pinned against an actual old guest, not assumed).
    let payload = exchange.request(
        "prepare",
        serde_json::json!({"session_id": session_id}),
        timeout,
    )?;
    let response: PrepareResponse = serde_json::from_value(payload).map_err(|_| {
        CistellaError::Contract("bad prepare response: shape violation".to_string())
    })?;
    let provenance = Provenance::Extension(guest_id.to_string());
    let credentials = admit_all(&response.credentials)?;
    let plan = build_plan(response, &provenance)?;
    Ok((plan, credentials))
}

/// Extension binary name resolved sibling-relative to the driver.
pub const EXTENSION_BIN: &str = "cistella-extension-landlock";

/// Tmpdir extension binary name, resolved the same sibling way.
pub const TMPDIR_BIN: &str = "cistella-extension-tmpdir";

/// Role capability offered to the extension guest.
const EXTENSION_ROLE: &str = "landlock";

/// Role capability offered to the tmpdir guest.
const TMPDIR_ROLE: &str = "tmpdir";

/// Contribution capability the Landlock hook exercises.
const EXTENSION_HOOKS: &str = "guest-hooks";

/// Contribution capabilities the host centrally supports: the full
/// merge-admission set. Offered to registered extensions alongside
/// their recorded role so a non-hook extension can contribute
/// environment, mounts, policy claims, or credentials — gating
/// stays central (contribution validators, credential admission,
/// lattice evaluation). The offer only decides what the guest may
/// advertise without tripping the closed-negotiation subset check;
/// hooks stay optional (only the recorded role is required).
const REGISTERED_OFFERED: [&str; 7] = [
    "environment",
    "mounts",
    "permissions",
    "policy-claims",
    "guest-hooks",
    "cleanup",
    "credentials",
];

/// Which guest answers one prepare transaction: a shipped
/// extension (Landlock confinement or tmpdir host state,
/// sibling-relative discovery) or one resolved registered entry
/// (verified bytes plus recorded role, resolved before prepare so
/// verification failures refuse pre-create).
#[derive(Debug)]
pub enum ExtensionSpawn {
    /// Shipped Landlock extension (sibling-relative discovery).
    Landlock,
    /// Shipped tmpdir extension (sibling-relative discovery).
    Tmpdir,
    /// Third-party extension (verified open FD, static argv).
    Registered(ResolvedGuest),
}

/// Invitation for one extension spawn: the provenance identity,
/// the host-supported hello offer (closed negotiation ceiling),
/// and the advertisements the runner requires before planning.
/// Shared by the single-transaction runner and pair composition
/// so both negotiate identically.
pub(crate) fn spawn_invitation(spawn: &ExtensionSpawn) -> (String, Vec<String>, Vec<String>) {
    match spawn {
        ExtensionSpawn::Landlock => (
            EXTENSION_BIN.to_string(),
            vec![EXTENSION_ROLE.to_string(), EXTENSION_HOOKS.to_string()],
            vec![EXTENSION_HOOKS.to_string()],
        ),
        ExtensionSpawn::Tmpdir => {
            // Shipped tmpdir negotiates like a registered guest
            // (role plus the full contribution set, hooks
            // optional) minus verification — install-dir trust,
            // same as the landlock sibling.
            let mut offered = vec![TMPDIR_ROLE.to_string()];
            offered.extend(REGISTERED_OFFERED.iter().map(|name| name.to_string()));
            (
                TMPDIR_BIN.to_string(),
                offered,
                vec![TMPDIR_ROLE.to_string()],
            )
        }
        ExtensionSpawn::Registered(resolved) => {
            let mut offered = vec![resolved.role.clone()];
            offered.extend(REGISTERED_OFFERED.iter().map(|name| name.to_string()));
            (resolved.name.clone(), offered, vec![resolved.role.clone()])
        }
    }
}

/// Runs one extension prepare transaction: hosts the guest
/// per-phase, admits on the required capabilities, merges centrally
/// against the framework-owned baseline, and shuts the guest down.
/// Hook staging and execution are the caller's job; hooks return
/// staged for the caller to hold.
///
/// `extra_args` rides the guest spawn (production passes none;
/// tests select deterministic peer modes). A shipped spawn
/// requires its role advertisement (landlock additionally
/// requires `guest-hooks` — its selection guarantees
/// engagement); a registered spawn requires only its recorded
/// role string (hooks stay optional — a zero-hook answer
/// converges plain downstream).
///
/// Admission checks the contribution type, not the role string:
/// the pinned binary name plus the install-directory trust anchor
/// already carry role identity for shipped guests (and the verified
/// registry entry for registered ones), and only the hook
/// contribution gates confinement behavior.
///
/// Residue dominance: a failed shutdown replaces the prepare
/// outcome; only a clean shutdown preserves it.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on discovery, admission, or
/// merge refusal, and `CistellaError::Protocol` on transport or
/// shutdown failure. Nine parameters mirror the transaction inputs
/// one-to-one (spawn, discovery base, argv, profile, baseline,
/// policy, session, lease base); bundling would only move them.
#[allow(clippy::too_many_arguments)]
pub fn run_extension_prepare(
    spawn: ExtensionSpawn,
    exe_dir: &std::path::Path,
    extra_args: &[String],
    profile: &crate::profile::Profile,
    triples: &[MountTriple],
    policy: &PolicySet,
    session_id: &str,
    lease_base: &std::path::Path,
) -> Result<(EvaluatedPlan, Option<crate::lock::PrepareLease>)> {
    let (guest_id, offered, required) = spawn_invitation(&spawn);
    // Durable prepare lease before the first spawn: quiescence is
    // unproven from here on, so every error path below either
    // proves it (clean shutdowns, which drop the handle) or
    // retains the uncertainty record (residue and post-spawn
    // hosting failures, which leak it for stale-gc).
    let mut lease = crate::lock::PrepareLease::create(lease_base, session_id)?;
    // Pending spawn recorded BEFORE hosting: a crash between
    // spawn and record_guest leaves gc skipping (never assuming),
    // cleared by the matching record below.
    lease.mark_pending()?;
    let (mut guest, negotiated) = match match spawn {
        ExtensionSpawn::Landlock => host_external(
            exe_dir,
            EXTENSION_BIN,
            extra_args,
            &offered,
            Deadlines::default(),
        ),
        ExtensionSpawn::Tmpdir => host_external(
            exe_dir,
            TMPDIR_BIN,
            extra_args,
            &offered,
            Deadlines::default(),
        ),
        ExtensionSpawn::Registered(resolved) => {
            host_registered(resolved, extra_args, &offered, Deadlines::default())
        }
    } {
        Ok((guest, negotiated)) => (guest, negotiated),
        Err(error) => {
            lease.retain();
            return Err(error);
        }
    };
    // The guest is hosted: record its pid plus process-group id
    // (the group leads from the spawned pid by pre_exec
    // setpgid). gc observes this identity without signaling.
    if let Err(error) = lease.record_guest(guest.pid(), guest.pid()) {
        lease.retain();
        return Err(error);
    }
    for required_capability in &required {
        if !negotiated.contains(required_capability) {
            if let Err(residue) = guest.shutdown() {
                lease.retain();
                return Err(residue);
            }
            return Err(CistellaError::Contract(format!(
                "extension guest {guest_id} must advertise {required_capability}"
            )));
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
    let outcome = run_prepare(
        guest.exchange_mut(),
        &guest_id,
        &negotiated,
        &context,
        policy,
        &acceptances,
        Deadlines::default().plan,
        session_id,
    );
    let shutdown = guest.shutdown();
    let plan = match (outcome, shutdown) {
        (Ok(plan), Ok(())) => plan,
        (_, Err(residue)) => {
            lease.retain();
            return Err(residue);
        }
        (Err(error), Ok(())) => return Err(error),
    };
    // Artifact-executable binding invariant: the hook executable must be exactly the staged
    // wrapper path — a correctly digest-pinned artifact with
    // `argv_prefix[0]` naming `/bin/sh` (or any other absolute
    // executable) would bypass confinement at composition. The
    // prepare payload carries no session context, so session-blind
    // extension args are never legitimate either: the prefix is the
    // singleton staged path, and the framework composes all wrapper
    // arguments at launch. Raw argv crosses verbatim by
    // exec (no shell), so no byte-class filtering applies — the gate
    // is structural identity, not content.
    for hook in &plan.merged.guest_hooks {
        check_hook_executable(hook)?;
    }
    Ok((plan, Some(lease)))
}

/// Driver-injected env names reserved against extension
/// contributions (unit-baked HOME, ssh-agent pointer, exec-time TERM).
pub(crate) const DRIVER_ENV: [&str; 3] = ["HOME", "SSH_AUTH_SOCK", "TERM"];

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
            placement: Placement {
                host_source: wire.host_source.clone(),
                container_target: wire.container_target.clone(),
            },
            provenance: provenance.clone(),
        });
    }
    let mut permissions = Vec::with_capacity(response.permissions.len());
    for wire in &response.permissions {
        permissions.push(PermissionContribution {
            target: wire.target.clone(),
            rights: Rights::parse_wire(&wire.rights)?,
            provenance: provenance.clone(),
        });
    }
    for path in &response.cleanup {
        if !path.starts_with('/') {
            return Err(CistellaError::Contract(format!(
                "cleanup declaration must be absolute: {path}"
            )));
        }
    }
    Ok(PreparePlan {
        permissions,
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
        cleanup: response.cleanup,
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
///   at `/src/state`) alike. Declarations
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
/// - Guest `/tmp` carries FULL rights unconditionally under
///   hook (operator direction — admitted by construction,
///   whatever backs it). Landlock permission only: retained
///   Podman RO bindings and filesystem modes still deny, and
///   RO-elsewhere aliases of a host-backed `/tmp` refuse
///   pre-create (baseline alias accounting).
/// - `$HOME` (container-private tmpfs) and `/dev/null` writes
///   stay denied: dogfood promotes only on evidence.
///   Entries deduplicate (first occurrence wins); harness argv
///   appends verbatim after `--`.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on hook count, empty routes,
/// or untranslatable roots.
pub fn compose_hook_argv(
    hooks: &[GuestHookRequest],
    placements: &[crate::mount::Placement],
    effective: &EffectivePermissions,
    ancestor_host: &std::path::Path,
    subtree_host: &std::path::Path,
    harness_argv: &[String],
) -> Result<Vec<String>> {
    let grants = crate::framework::landlock_compiler::compute_grants(
        hooks,
        placements,
        effective,
        ancestor_host,
        subtree_host,
    )?;
    let hook = &hooks[0];
    let mut argv = Vec::new();
    argv.extend(hook.argv_prefix.iter().cloned());
    // System baseline first (fixed position, deterministic):
    // read-execute on the container rootfs, full rights on the
    // fixed FULL baselines (rendered from the shared set the
    // revision accounts, so composition and retention agree).
    argv.push("--allow-ro=/".to_string());
    for baseline in crate::framework::landlock_compiler::FULL_BASELINE_ROUTES {
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
