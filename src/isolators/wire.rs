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
//! unknown handle plus an unlocated key creates.
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
use crate::framework::contract::{CancelFlag, ExecutionHandle, ReconciliationKey, UnitHandle};
use crate::framework::isolator::{CreateSpec, ExecutionOutcome, Isolator, StdioBinding};
use crate::isolators::podman::PodmanIsolator;

/// Wire op names (mirror the isolator-contract schemas).
pub const OP_CREATE: &str = "isolator.create";
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
}

/// Framework unit binding: local handle plus the attempt identity
/// that created it.
#[derive(Debug, Clone)]
struct UnitBinding {
    /// Local backend handle.
    local: UnitHandle,
    /// Reconciliation key of the creating attempt.
    key: ReconciliationKey,
    /// Creation spec of the creating attempt.
    spec: CreateSpec,
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
        }
    }

    /// Attaches the ancillary-fd channel bundles arrive on
    /// (production guest; the rendezvous socket after connect).
    #[must_use]
    pub fn with_fd_channel(mut self, channel: std::os::fd::OwnedFd) -> Self {
        self.fd_channel = Some(channel);
        self
    }

    /// True when any framework handle is bound (live session state).
    #[must_use]
    pub fn has_live_state(&self) -> bool {
        let tables = self.tables.lock().expect("guest table lock");
        !(tables.units.is_empty() && tables.executions.is_empty())
    }

    /// Converges every bound unit without live executions to clean,
    /// best-effort in table order, then drops bindings for units
    /// that converged.
    ///
    /// Units with live executions are left running and bound: a
    /// disconnect is detach-without-kill, and only the framework's
    /// typed teardown path may end them (never a guest-side sweep).
    /// Units that fail terminate or remove keep their bindings, so
    /// a second call still has handles for another attempt.
    /// Executions follow their unit: cleared only when their unit
    /// converges. The first backend failure reports.
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
                .map(|(fw, binding)| (fw.clone(), binding.local.clone(), binding.key.clone()))
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

    /// Resolves a framework unit handle to its local handle.
    ///
    /// Grammar-checked before lookup: only minted-shape strings
    /// reach the table, and diagnostics echo validated handles or
    /// fixed messages, never raw wire bytes.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on grammar violations or
    /// unknown handles, before any backend call.
    fn resolve_unit(&self, handle: &UnitHandle) -> Result<UnitHandle> {
        check_handle_grammar("unit", handle.as_str())?;
        self.tables
            .lock()
            .expect("guest table lock")
            .units
            .get(handle.as_str())
            .map(|binding| binding.local.clone())
            .ok_or_else(|| {
                CistellaError::Contract(format!("unknown unit handle: {}", handle.as_str()))
            })
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
                        // Replay guard: the same handle replays only
                        // the identical attempt; a different key or
                        // spec refuses instead of rebinding the unit
                        // out from under the first attempt.
                        if binding.key != req.reconciliation_key || binding.spec != req.spec {
                            return Err(CistellaError::Contract(format!(
                                "unit handle bound to a different attempt: {fw}"
                            )));
                        }
                        return Ok(serde_json::json!({"unit_handle": fw}));
                    }
                }
                let local = match self.backend.locate(&req.reconciliation_key)? {
                    Some(found) => found,
                    None => self.backend.create(&req.spec, &req.reconciliation_key)?,
                };
                self.tables.lock().expect("guest table lock").units.insert(
                    fw.clone(),
                    UnitBinding {
                        local,
                        key: req.reconciliation_key,
                        spec: req.spec,
                    },
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
                let req: LaunchWire = parse(op, payload)?;
                // Channel first: without it harness stdio has no
                // route, so the launch refuses before lookup.
                if self.fd_channel.is_none() {
                    return Err(CistellaError::Contract(
                        "launch requires an fd channel".to_string(),
                    ));
                }
                let local = self.resolve_unit(&req.unit_handle)?;
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
                let launched = self.backend.execute_launch(
                    &local,
                    &req.argv,
                    req.workdir.as_deref(),
                    StdioBinding::HeldFiles {
                        stdin,
                        stdout,
                        stderr,
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
                let local = self.resolve_unit(&req.unit_handle)?;
                let snapshot = self.backend.inspect(&local)?;
                Ok(serde_json::to_value(snapshot).expect("snapshot serializes"))
            }
            OP_STATE => {
                let req: InspectWire = parse(op, payload)?;
                let local = self.resolve_unit(&req.unit_handle)?;
                let state = self.backend.state(&local)?;
                Ok(serde_json::json!({"lifecycle":
                    serde_json::to_value(state).expect("state serializes")}))
            }
            OP_TERMINATE => {
                let req: TerminateWire = parse(op, payload)?;
                let local = self.resolve_unit(&req.unit_handle)?;
                let grace = Duration::from_millis(req.grace_ms);
                let attestation = self
                    .backend
                    .terminate(&local, grace, &req.reconciliation_key)?;
                Ok(serde_json::json!({"stopped_attestation":
                    serde_json::to_value(attestation).expect("attestation serializes")}))
            }
            OP_REMOVE => {
                let req: HandleKeyWire = parse(op, payload)?;
                let local = self.resolve_unit(&req.unit_handle)?;
                let attestation = self.backend.remove(&local, &req.reconciliation_key)?;
                // Evict only after the backend confirms removal: the
                // binding dies with the unit, while a failed remove
                // keeps its handle for retry/reconciliation.
                {
                    let mut tables = self.tables.lock().expect("guest table lock");
                    tables.units.remove(req.unit_handle.as_str());
                }
                {
                    let mut tables = self.tables.lock().expect("guest table lock");
                    let remaining: std::collections::HashSet<String> =
                        tables.units.keys().cloned().collect();
                    tables
                        .executions
                        .retain(|_, binding| remaining.contains(&binding.unit));
                }
                Ok(serde_json::json!({"removed_attestation":
                    serde_json::to_value(attestation).expect("attestation serializes")}))
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
