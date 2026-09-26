//! Podman isolator wire dispatch (task 2.1).
//!
//! Translates `isolator.*` wire operations into [`Isolator`] trait
//! calls against the in-process [`PodmanIsolator`], mapping
//! framework-issued opaque handle strings to local trait handles.
//! The external guest binary (`cistella-isolator-podman`) owns the
//! stdio loop; this module owns op semantics, so both sides share
//! one dispatch and cannot drift.
//!
//! Handle discipline: the framework mints `unit_handle` /
//! `execution_handle` strings and the guest never invents them —
//! unknown framework handles refuse with a typed error before any
//! backend call (fast-testable without podman). `create` with a
//! known framework handle replays idempotently; with an unknown
//! handle but a known reconciliation key the guest adopts the
//! located unit (re-exec convergence after guest restart); only an
//! unknown handle plus an unlocated key creates. Successful
//! `remove` evicts the live binding into a removal tombstone keyed
//! by the original handle: identical convergent retries converge
//! residue-free through idempotent backend cleanup (never
//! resurrection), divergent live attempts — including same-handle
//! create — refuse typed, and the retained local reaches only the
//! backend tombstone.
//!
//! Awaiting is synchronous here: the guest binary wraps slow
//! `await_result` calls with `{pending}` ticker frames at the
//! stdio layer. Cancellation across the wire is stream abandonment
//! (the host drops the connection to detach); the guest awaits
//! with its own uncancelled flag and the outcome stays redeemable
//! until `remove`.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{
    CancelFlag, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use crate::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, StdioBinding, UnitSnapshot,
};
use crate::isolators::podman::PodmanIsolator;

/// Wire op names (mirror the isolator-contract schemas).
pub const OP_CREATE: &str = "isolator.create";
/// Best-effort drain budget for a queued bundle on early refusal.
/// The framework sends bundles before ops, so a queued bundle is
/// normally immediate; the budget only bounds a dead framework.
const DRAIN_BUDGET: Duration = Duration::from_secs(2);
pub const OP_INITIATE: &str = "isolator.initiate";
pub const OP_EXECUTE_LAUNCH: &str = "isolator.execute_launch";
pub const OP_INSPECT: &str = "isolator.inspect";
pub const OP_STATE: &str = "isolator.state";
pub const OP_TERMINATE: &str = "isolator.terminate";
pub const OP_REMOVE: &str = "isolator.remove";

/// `isolator.create` payload: framework handle plus spec plus key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWire {
    /// Framework-issued unit handle (never guest-chosen).
    unit_handle: UnitHandle,
    /// Creation spec (built and validated framework-side).
    spec: CreateSpec,
    /// Reconciliation key for adopt-or-create convergence.
    reconciliation_key: ReconciliationKey,
}

/// Handle-plus-key payload shape shared by initiate/terminate/remove.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleKeyWire {
    /// Framework-issued unit handle.
    unit_handle: UnitHandle,
    /// Reconciliation key for this attempt.
    reconciliation_key: ReconciliationKey,
}

/// `isolator.execute_launch` payload.
///
/// Carries no stdio: harness descriptors arrive over the
/// ancillary-fd channel in a header-bound bundle, never in JSON
/// (file descriptors are not serializable, and the external path
/// offers no `Inherit` to name).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchWire {
    /// Framework-issued unit handle.
    unit_handle: UnitHandle,
    /// Framework-issued execution handle to bind.
    execution_handle: ExecutionHandle,
    /// Harness argv.
    argv: Vec<String>,
    /// Launch workdir (session worktree target); null runs image default.
    workdir: Option<String>,
    /// Reconciliation key for this attempt.
    reconciliation_key: ReconciliationKey,
}

/// `isolator.await_result` payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AwaitWire {
    /// Framework-issued execution handle to redeem.
    execution_handle: ExecutionHandle,
}

/// `isolator.inspect` / `isolator.state` payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectWire {
    /// Framework-issued unit handle.
    unit_handle: UnitHandle,
}

/// `isolator.terminate` grace payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminateWire {
    /// Framework-issued unit handle.
    unit_handle: UnitHandle,
    /// SIGTERM grace in milliseconds.
    grace_ms: u64,
    /// Reconciliation key for this attempt.
    reconciliation_key: ReconciliationKey,
}

/// Parses one wire payload with unknown-field refusal.
///
/// Shape violations carry a fixed message: serde error text can
/// echo guest-controlled bytes, so it never reaches diagnostics.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shape violations.
fn parse<Wire: for<'de> Deserialize<'de>>(op: &str, payload: &serde_json::Value) -> Result<Wire> {
    serde_json::from_value(payload.clone())
        .map_err(|_| CistellaError::Contract(format!("bad {op} payload: shape violation")))
}

/// Framework-handle grammar: minted shape only (`[a-z0-9]`, bounded).
/// A wire handle outside this grammar refuses with a fixed message
/// before table lookup, so untrusted bytes never flow into
/// diagnostics or backend calls.
fn check_handle_grammar(kind: &str, handle: &str) -> Result<()> {
    let ok = !handle.is_empty()
        && handle.len() <= 128
        && handle
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !ok {
        return Err(CistellaError::Contract(format!(
            "bad {kind} handle: shape violation"
        )));
    }
    Ok(())
}

/// Guest-side dispatch: one backend plus framework-handle tables.
///
/// The backend is generic over [`Isolator`] so conformance can
/// inject a scripted backend for replay proofs without Podman;
/// production instantiates the [`PodmanIsolator`] default. Tables
/// live behind one mutex while the backend stays directly owned:
/// resolving a handle clones the local handle under the lock, and
/// the blocking backend call runs lock-free, so `terminate` served
/// on another thread is never stuck behind a live `await`.
///
/// Replay binding: every framework handle is bound to the key (and
/// request identity) that created it. A known handle with a
/// different key or divergent request refuses as mismatched reuse
/// instead of overwriting — overwriting would orphan a live local
/// handle (e.g. a second harness on retry). A known handle with
/// the identical key and request replays idempotently without
/// re-executing.
pub struct IsolatorGuest<B = PodmanIsolator> {
    /// Backend engine (conformance reference and guest engine).
    backend: B,
    /// Framework-handle tables (units plus executions).
    tables: std::sync::Mutex<Tables>,
    /// Ancillary-fd channel for launch stdio bundles (production
    /// guest only; absent in unit tests, where launches refuse).
    fd_channel: Option<std::os::fd::OwnedFd>,
    /// Conductor pid recorded at guest startup (the guest's parent
    /// is conduct while the session lives). Launch-time parentage
    /// must still equal this: a subreaper adoption passes a
    /// `parent != 1` check but is not conduct, so equality — not
    /// non-init — authenticates the foreground pgid source.
    host_pid: u32,
}

/// Live backend unit plus the attempt identity that created it.
#[derive(Debug, Clone)]
struct LiveBinding {
    /// Local backend handle.
    local: UnitHandle,
    /// Reconciliation key of the creating attempt.
    key: ReconciliationKey,
    /// Creation spec of the creating attempt.
    spec: CreateSpec,
}

/// Framework unit binding: live backend handle plus the attempt
/// identity that created it — or a removal tombstone. Eviction on
/// remove clears the live binding (no handle resurrection); the
/// tombstone retains the local for idempotent cleanup delegation
/// plus the unit name for absent attestations, while same-handle
/// create refuses instead of resurrecting.
#[derive(Debug, Clone)]
enum UnitBinding {
    /// Live backend unit (boxed: the spec dwarfs the tombstone).
    Live(Box<LiveBinding>),
    /// Removal tombstone: live binding evicted, absence recorded.
    /// Keyed by the original framework handle (the map key); the
    /// retained local reaches the backend tombstone for idempotent
    /// cleanup delegation only, and the identity feeds absent
    /// attestations without backend reads.
    Removed {
        /// Local backend handle, retained ONLY for idempotent
        /// cleanup delegation (`terminate`/`remove` re-runs clear
        /// residue without resurrecting state; never resolved for
        /// live operations).
        local: UnitHandle,
        /// Unit identity (container name) for attestations.
        unit_identity: String,
    },
}

/// Framework execution binding: local handle plus attempt identity.
#[derive(Debug, Clone)]
struct ExecBinding {
    /// Local backend handle.
    local: ExecutionHandle,
    /// Framework unit handle this execution was launched on.
    unit: String,
    /// Reconciliation key of the launching attempt.
    key: ReconciliationKey,
    /// Harness argv of the launching attempt.
    argv: Vec<String>,
    /// Launch workdir of the launching attempt.
    workdir: Option<String>,
}

/// Framework-handle tables keyed by framework-issued strings.
#[derive(Debug, Default)]
struct Tables {
    /// Framework unit-handle string to binding.
    units: HashMap<String, UnitBinding>,
    /// Framework execution-handle string to binding.
    executions: HashMap<String, ExecBinding>,
}

impl IsolatorGuest<PodmanIsolator> {
    /// Empty dispatch with the Podman backend (no bound handles, no
    /// fd channel).
    #[must_use]
    pub fn new() -> Self {
        Self {
            backend: PodmanIsolator::new(),
            tables: std::sync::Mutex::new(Tables::default()),
            fd_channel: None,
            host_pid: std::os::unix::process::parent_id(),
        }
    }
}

impl<B: Isolator> IsolatorGuest<B> {
    /// Dispatch with an injectable backend (replay proofs).
    #[must_use]
    pub fn with_backend(backend: B) -> Self {
        Self {
            backend,
            tables: std::sync::Mutex::new(Tables::default()),
            fd_channel: None,
            host_pid: std::os::unix::process::parent_id(),
        }
    }

    /// Best-effort drain of one queued bundle: the framework sends
    /// the bundle before the op, so an op that refuses before
    /// consuming leaves a stale bundle for the next launch. Drained
    /// fds close on drop; drain failure is ignored because the
    /// original refusal reports either way.
    fn drain_one_bundle(&self) {
        if let Some(channel) = self.fd_channel.as_ref() {
            let _ = crate::framework::fdpass::recv_bundle(channel, DRAIN_BUDGET);
        }
    }

    /// Attaches the ancillary-fd channel bundles arrive on
    /// (production guest; the rendezvous socket after connect).
    #[must_use]
    pub fn with_fd_channel(mut self, channel: std::os::fd::OwnedFd) -> Self {
        self.fd_channel = Some(channel);
        self
    }

    /// True when any framework handle is live (bound units or
    /// executions). Tombstones are absence records, not live
    /// state: a guest holding only tombstones exits quietly on
    /// clean EOF.
    #[must_use]
    pub fn has_live_state(&self) -> bool {
        let tables = self.tables.lock().expect("guest table lock");
        let live_unit = tables
            .units
            .values()
            .any(|binding| matches!(binding, UnitBinding::Live(_)));
        live_unit || !tables.executions.is_empty()
    }

    /// Converges every LIVE bound unit without live executions to
    /// clean, best-effort in table order, then drops the converged
    /// bindings. Tombstones are already clean and never re-enter
    /// the sweep; units with live executions are left running and
    /// bound (disconnect is detach-without-kill, and only the
    /// framework's typed teardown path may end them). Dropped
    /// (not tombstoned): the sweep runs at disconnect, and the
    /// guest exits right after, so no live guest serves those
    /// handles again — repeatability across the sweep is the fresh
    /// guest's adopt-or-create, keyed by reconciliation key.
    /// Units that fail terminate or remove keep their live
    /// bindings, so a second call still has handles for another
    /// attempt. Executions follow their unit: cleared only when
    /// their unit converges. The first backend failure reports.
    ///
    /// # Errors
    ///
    /// Returns the first backend failure, if any.
    pub fn converge_all(&self, grace: Duration) -> Result<()> {
        let bindings: Vec<(String, UnitHandle, ReconciliationKey)> = {
            let tables = self.tables.lock().expect("guest table lock");
            let live_units: std::collections::HashSet<String> = tables
                .executions
                .values()
                .map(|binding| binding.unit.clone())
                .collect();
            tables
                .units
                .iter()
                .filter(|(fw, _)| !live_units.contains(*fw))
                .filter_map(|(fw, binding)| match binding {
                    UnitBinding::Live(live) => {
                        Some((fw.clone(), live.local.clone(), live.key.clone()))
                    }
                    UnitBinding::Removed { .. } => None,
                })
                .collect()
        };
        let mut cleared: Vec<String> = Vec::new();
        let mut first_error: Option<CistellaError> = None;
        for (fw, local, key) in &bindings {
            // Best-effort per unit: a failed terminate still attempts
            // remove, and a failed unit never stops the sweep. Only
            // fully converged units clear; the first real backend
            // error reports with its class intact.
            let terminated = self.backend.terminate(local, grace, key);
            let removed = self.backend.remove(local, key);
            if terminated.is_ok() && removed.is_ok() {
                cleared.push(fw.clone());
            } else if first_error.is_none() {
                first_error = terminated.err().or(removed.err());
            }
        }
        {
            let mut tables = self.tables.lock().expect("guest table lock");
            for fw in &cleared {
                tables.units.remove(fw);
            }
        }
        {
            let mut tables = self.tables.lock().expect("guest table lock");
            let remaining: std::collections::HashSet<String> =
                tables.units.keys().cloned().collect();
            tables
                .executions
                .retain(|_, binding| remaining.contains(&binding.unit));
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Looks up a framework unit handle's binding (live or
    /// tombstone).
    ///
    /// Grammar-checked before lookup: only minted-shape strings
    /// reach the table, and diagnostics echo validated handles or
    /// fixed messages, never raw wire bytes.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on grammar violations or
    /// unknown handles, before any backend call.
    fn lookup_unit(&self, handle: &UnitHandle) -> Result<UnitBinding> {
        check_handle_grammar("unit", handle.as_str())?;
        self.tables
            .lock()
            .expect("guest table lock")
            .units
            .get(handle.as_str())
            .cloned()
            .ok_or_else(|| {
                CistellaError::Contract(format!("unknown unit handle: {}", handle.as_str()))
            })
    }

    /// Resolves a framework unit handle to its local handle.
    ///
    /// Grammar-checked before lookup: only minted-shape strings
    /// reach the table, and diagnostics echo validated handles or
    /// fixed messages, never raw wire bytes.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on grammar violations,
    /// unknown handles, or removed handles (tombstones answer
    /// through their own arms, never by resurrecting a backend
    /// handle), before any backend call.
    fn resolve_unit(&self, handle: &UnitHandle) -> Result<UnitHandle> {
        match self.lookup_unit(handle)? {
            UnitBinding::Live(live) => Ok(live.local.clone()),
            UnitBinding::Removed { .. } => Err(CistellaError::Contract(format!(
                "unit handle removed: {}",
                handle.as_str()
            ))),
        }
    }

    /// Resolves a framework execution handle to its local handle.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on grammar violations or
    /// unknown handles, before any backend call.
    fn resolve_execution(&self, handle: &ExecutionHandle) -> Result<ExecutionHandle> {
        check_handle_grammar("execution", handle.as_str())?;
        self.tables
            .lock()
            .expect("guest table lock")
            .executions
            .get(handle.as_str())
            .map(|binding| binding.local.clone())
            .ok_or_else(|| {
                CistellaError::Contract(format!("unknown execution handle: {}", handle.as_str()))
            })
    }

    /// Dispatches one wire operation to its payload value.
    ///
    /// Unknown ops, malformed payloads, and unknown handles refuse
    /// with typed errors; backend failures propagate as-is for the
    /// stdio layer to envelope. Shared-reference dispatch: table
    /// access locks briefly per operation while blocking backend
    /// calls run lock-free, so concurrent operations never wedge
    /// behind a live await.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on routing/shape/handle
    /// failures and the backend error otherwise.
    pub fn dispatch(&self, op: &str, payload: &serde_json::Value) -> Result<serde_json::Value> {
        match op {
            OP_CREATE => {
                let req: CreateWire = parse(op, payload)?;
                // Incoming framework handles are grammar-checked
                // before binding or rendering: a mismatched-reuse
                // diagnostic echoes the handle, so only
                // minted-shape strings ever reach that path.
                check_handle_grammar("unit", req.unit_handle.as_str())?;
                let fw = req.unit_handle.as_str().to_string();
                {
                    let tables = self.tables.lock().expect("guest table lock");
                    if let Some(binding) = tables.units.get(&fw) {
                        match binding {
                            UnitBinding::Live(live) => {
                                // Replay guard: the same handle replays only
                                // the identical attempt; a different key or
                                // spec refuses instead of rebinding the unit
                                // out from under the first attempt.
                                if live.key != req.reconciliation_key || live.spec != req.spec {
                                    return Err(CistellaError::Contract(format!(
                                        "unit handle bound to a different attempt: {fw}"
                                    )));
                                }
                                return Ok(serde_json::json!({"unit_handle": fw}));
                            }
                            UnitBinding::Removed { .. } => {
                                // No re-create over a tombstone: the
                                // handle's lifecycle ended at remove,
                                // and a same-handle replay must not
                                // resurrect the unit or rerun its side
                                // effects. Fresh units arrive on fresh
                                // handles (conduct mints per attempt;
                                // adopt-or-create keys those); a stale
                                // same-handle retry refuses instead.
                                return Err(CistellaError::Contract(format!(
                                    "unit handle removed: {fw}"
                                )));
                            }
                        }
                    }
                }
                let local = match self.backend.locate(&req.reconciliation_key)? {
                    Some(found) => found,
                    None => self.backend.create(&req.spec, &req.reconciliation_key)?,
                };
                self.tables.lock().expect("guest table lock").units.insert(
                    fw.clone(),
                    UnitBinding::Live(Box::new(LiveBinding {
                        local,
                        key: req.reconciliation_key,
                        spec: req.spec,
                    })),
                );
                Ok(serde_json::json!({"unit_handle": fw}))
            }
            OP_INITIATE => {
                let req: HandleKeyWire = parse(op, payload)?;
                let local = self.resolve_unit(&req.unit_handle)?;
                let attestation = self.backend.initiate(&local, &req.reconciliation_key)?;
                Ok(serde_json::json!({"started_attestation":
                    serde_json::to_value(attestation).expect("attestation serializes")}))
            }
            OP_EXECUTE_LAUNCH => {
                let req: LaunchWire = match parse(op, payload) {
                    Ok(req) => req,
                    Err(error) => {
                        self.drain_one_bundle();
                        return Err(error);
                    }
                };
                // Channel first: without it harness stdio has no
                // route, so the launch refuses before lookup.
                if self.fd_channel.is_none() {
                    return Err(CistellaError::Contract(
                        "launch requires an fd channel".to_string(),
                    ));
                }
                let local = match self.resolve_unit(&req.unit_handle) {
                    Ok(local) => local,
                    Err(error) => {
                        self.drain_one_bundle();
                        return Err(error);
                    }
                };
                check_handle_grammar("execution", req.execution_handle.as_str())?;
                let fw = req.execution_handle.as_str().to_string();
                {
                    let tables = self.tables.lock().expect("guest table lock");
                    if let Some(binding) = tables.executions.get(&fw) {
                        // Idempotent replay: the identical attempt
                        // returns its binding without spawning again;
                        // any divergence refuses instead of
                        // overwriting (and orphaning) a live child.
                        // The replay still consumes its staged bundle
                        // (validating the header) so the channel
                        // stays aligned: an unread bundle would
                        // desynchronize the next launch.
                        if binding.key == req.reconciliation_key
                            && binding.argv == req.argv
                            && binding.workdir == req.workdir
                        {
                            drop(tables);
                            let channel = self
                                .fd_channel
                                .as_ref()
                                .expect("channel present: this binding was created through it");
                            let (bundle, _) = crate::framework::fdpass::recv_bundle(
                                channel,
                                crate::framework::fdpass::LAUNCH_BUNDLE_WAIT,
                            )?;
                            if bundle.unit_handle != req.unit_handle.as_str()
                                || bundle.execution_handle != fw
                            {
                                return Err(CistellaError::Contract(
                                    "fd bundle bound to a different launch".to_string(),
                                ));
                            }
                            return Ok(serde_json::json!({"execution_handle": fw}));
                        }
                        return Err(CistellaError::Contract(format!(
                            "execution handle bound to a different attempt: {fw}"
                        )));
                    }
                }
                // Harness stdio arrives over the fd channel in a
                // header-bound bundle — never inherited (the guest's
                // own stdio is protocol pipes) and never by
                // pathname. The bundle header must name this exact
                // unit plus execution; anything else refuses before
                // spawn. Channel presence was checked above.
                let channel = self.fd_channel.as_ref().expect("fd channel checked above");
                let (bundle, fds) = crate::framework::fdpass::recv_bundle(
                    channel,
                    crate::framework::fdpass::LAUNCH_BUNDLE_WAIT,
                )?;
                if bundle.unit_handle != req.unit_handle.as_str() || bundle.execution_handle != fw {
                    return Err(CistellaError::Contract(
                        "fd bundle bound to a different launch".to_string(),
                    ));
                }
                let [stdin, stdout, stderr] = fds;
                // Conductor identity (pid, foreground pgid) with
                // verified parentage: evaluated HERE in the guest
                // (whose parent is conduct while the session lives),
                // never one generation deeper where getppid returns
                // the guest itself. A failed lookup maps to None;
                // the TTY branch turns that into a typed refusal
                // downstream rather than a silent background launch.
                // The pid travels so pre_exec can verify the
                // conductor's CURRENT pgid against the captured one
                // (comparing the guest's own pgid would differ by
                // design and refuse every launch).
                let parent = std::os::unix::process::parent_id();
                let pgid = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(parent as i32)))
                    .ok()
                    .map(|pgid| pgid.as_raw() as u32);
                let conductor = crate::framework::isolator::verified_conductor_pgid(
                    parent,
                    self.host_pid,
                    pgid,
                );
                let launched = self.backend.execute_launch(
                    &local,
                    &req.argv,
                    req.workdir.as_deref(),
                    StdioBinding::HeldFiles {
                        stdin,
                        stdout,
                        stderr,
                        conductor,
                    },
                    &req.reconciliation_key,
                )?;
                self.tables
                    .lock()
                    .expect("guest table lock")
                    .executions
                    .insert(
                        fw.clone(),
                        ExecBinding {
                            local: launched,
                            unit: req.unit_handle.as_str().to_string(),
                            key: req.reconciliation_key,
                            argv: req.argv,
                            workdir: req.workdir,
                        },
                    );
                Ok(serde_json::json!({"execution_handle": fw}))
            }
            crate::framework::protocol::AWAIT_RESULT_OP => {
                let req: AwaitWire = parse(op, payload)?;
                let local = self.resolve_execution(&req.execution_handle)?;
                let cancel = CancelFlag::new();
                let outcome: ExecutionOutcome = self.backend.await_result(&local, &cancel)?;
                // Schema shapes (`exit_status`/`signal`), not the
                // enum's derived spelling.
                match outcome {
                    ExecutionOutcome::Exited(code) => Ok(serde_json::json!({"exit_status": code})),
                    ExecutionOutcome::Signaled(signum) => Ok(serde_json::json!({"signal": signum})),
                }
            }
            OP_INSPECT => {
                let req: InspectWire = parse(op, payload)?;
                match self.lookup_unit(&req.unit_handle)? {
                    UnitBinding::Live(live) => {
                        let snapshot = self.backend.inspect(&live.local)?;
                        Ok(serde_json::to_value(snapshot).expect("snapshot serializes"))
                    }
                    UnitBinding::Removed { unit_identity, .. } => {
                        // Tombstone reports absence without backend
                        // contact: the unit is gone, so identity is
                        // retained while session/image details read
                        // empty (matching the reference backend's
                        // post-remove rendering).
                        let snapshot = UnitSnapshot {
                            unit_identity,
                            lifecycle: LifecycleState::Absent,
                            active_state: "inactive".to_string(),
                            session_id: String::new(),
                            image: String::new(),
                        };
                        Ok(serde_json::to_value(snapshot).expect("snapshot serializes"))
                    }
                }
            }
            OP_STATE => {
                let req: InspectWire = parse(op, payload)?;
                match self.lookup_unit(&req.unit_handle)? {
                    UnitBinding::Live(live) => {
                        let state = self.backend.state(&live.local)?;
                        Ok(serde_json::json!({"lifecycle":
                            serde_json::to_value(state).expect("state serializes")}))
                    }
                    UnitBinding::Removed { .. } => Ok(serde_json::json!({"lifecycle":
                        serde_json::to_value(LifecycleState::Absent)
                            .expect("state serializes")})),
                }
            }
            OP_TERMINATE => {
                let req: TerminateWire = parse(op, payload)?;
                match self.lookup_unit(&req.unit_handle)? {
                    UnitBinding::Live(live) => {
                        let grace = Duration::from_millis(req.grace_ms);
                        let attestation =
                            self.backend
                                .terminate(&live.local, grace, &req.reconciliation_key)?;
                        Ok(serde_json::json!({"stopped_attestation":
                            serde_json::to_value(attestation).expect("attestation serializes")}))
                    }
                    UnitBinding::Removed { local, .. } => {
                        // Repeatable teardown delegates to the
                        // backend tombstone: idempotent cleanup
                        // contact only (stray settle, never
                        // resurrection).
                        let grace = Duration::from_millis(req.grace_ms);
                        let attestation =
                            self.backend
                                .terminate(&local, grace, &req.reconciliation_key)?;
                        Ok(serde_json::json!({"stopped_attestation":
                            serde_json::to_value(attestation).expect("attestation serializes")}))
                    }
                }
            }
            OP_REMOVE => {
                let req: HandleKeyWire = parse(op, payload)?;
                let fw = req.unit_handle.as_str().to_string();
                match self.lookup_unit(&req.unit_handle)? {
                    UnitBinding::Live(live) => {
                        let attestation =
                            self.backend.remove(&live.local, &req.reconciliation_key)?;
                        // Evict the live binding into a tombstone
                        // only after the backend confirms removal; a
                        // failed remove keeps the live handle for
                        // retry/reconciliation. The tombstone retains
                        // the local handle for idempotent cleanup
                        // delegation below (never resolved for live
                        // operations). Executions die with the unit
                        // either way (never resurrected).
                        {
                            let mut tables = self.tables.lock().expect("guest table lock");
                            tables.units.insert(
                                fw.clone(),
                                UnitBinding::Removed {
                                    local: live.local.clone(),
                                    unit_identity: attestation.unit_identity.clone(),
                                },
                            );
                            tables.executions.retain(|_, binding| binding.unit != fw);
                        }
                        Ok(serde_json::json!({"removed_attestation":
                            serde_json::to_value(attestation).expect("attestation serializes")}))
                    }
                    UnitBinding::Removed { local, .. } => {
                        // Idempotent repeat delegates to the backend
                        // tombstone so surviving residue (e.g. scratch
                        // planted after the first remove) still
                        // converges; absence re-attested, nothing
                        // resurrected.
                        let attestation = self.backend.remove(&local, &req.reconciliation_key)?;
                        Ok(serde_json::json!({"removed_attestation":
                            serde_json::to_value(attestation).expect("attestation serializes")}))
                    }
                }
            }
            _ => Err(CistellaError::Contract(
                "unknown isolator operation".to_string(),
            )),
        }
    }
}

impl Default for IsolatorGuest<PodmanIsolator> {
    fn default() -> Self {
        Self::new()
    }
}
