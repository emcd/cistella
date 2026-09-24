//! Podman isolator: Quadlet-owned units behind the framework trait.
//!
//! Trait implementation plus backend state (handle tables, exec
//! tracking, state classification). Quadlet unit mechanics live in
//! [`super::quadlet`]; `conduct` and the other commands drive units
//! through [`Isolator`], never through direct `podman`/`systemctl`
//! calls (companion `enter` keeps the shared transport arg builders).
//!
//! Locking follows the trait contract: lifecycle methods assume the
//! caller holds the creation-window lock wherever the pre-extraction
//! code did. Process entry points that run outside the window
//! (`terminate` command, GC) acquire it before delegating.

use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

use crate::error::{CistellaError, Result};
use crate::framework::contract::{
    CancelFlag, Capability, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use crate::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use crate::framework::signals;
use crate::isolators::quadlet::{
    container_exists, generate_quadlet_unit, install_quadlet, quadlet_dir, query_unit_props,
    remove_scratch, remove_unit_file, start_quadlet, stop_settle,
};
use crate::lock::scratch_dir;
use crate::registry::unit_file_label;
use crate::session::{LABEL_ID, LABEL_RECONCILIATION_KEY};

/// Unit record tracked per issued handle.
#[derive(Debug, Clone)]
struct UnitRecord {
    /// Container name (unit identity).
    container_name: String,
    /// Session id (scratch ownership).
    session_id: String,
    /// Quadlet unit file name.
    unit_name: String,
}

/// Pending launched execution awaiting `await_result`.
///
/// The record (and, once collected, the outcome) lives until
/// `remove`: cancelling detaches without killing, and a later await
/// on the same handle re-attaches or replays. The table retains
/// identity only — the caller owns the reconciliation key for any
/// cross-call operation (terminate/remove take it as a parameter).
struct PendingExec {
    /// Unit this execution belongs to (cleared with the unit).
    unit: UnitHandle,
    /// Running harness child (None once reaped).
    child: Option<Child>,
    /// Collected outcome for replay until `remove`.
    outcome: Option<ExecutionOutcome>,
}

/// Podman isolator backend (`podman` + Quadlet + systemd user manager).
pub struct PodmanIsolator {
    /// Issued handles to unit records.
    units: Mutex<HashMap<UnitHandle, UnitRecord>>,
    /// Reconciliation keys to handles (replacement-peer recovery).
    keys: Mutex<HashMap<ReconciliationKey, UnitHandle>>,
    /// Launched executions awaiting result collection.
    executions: Mutex<HashMap<ExecutionHandle, PendingExec>>,
}

impl PodmanIsolator {
    /// Fresh backend with empty handle tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            units: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashMap::new()),
            executions: Mutex::new(HashMap::new()),
        }
    }

    /// Adopts a pre-existing unit (cross-process recovery): registers
    /// the container/session pair and returns a handle for it.
    ///
    /// Used by the `terminate` command and GC, which resolve live
    /// units from the registry rather than from an in-process table.
    pub fn adopt(&self, container_name: &str, session_id: &str) -> UnitHandle {
        let handle = UnitHandle::mint();
        let record = UnitRecord {
            container_name: container_name.to_string(),
            session_id: session_id.to_string(),
            unit_name: format!("{container_name}.container"),
        };
        self.units
            .lock()
            .expect("unit table lock")
            .insert(handle.clone(), record);
        handle
    }

    /// Looks up the record for a handle.
    fn record(&self, handle: &UnitHandle) -> Result<UnitRecord> {
        self.units
            .lock()
            .expect("unit table lock")
            .get(handle)
            .cloned()
            .ok_or_else(|| {
                CistellaError::Contract(format!("unknown unit handle: {}", handle.as_str()))
            })
    }

    /// Records a handle/key pair after successful creation.
    fn register(&self, handle: UnitHandle, record: UnitRecord, key: &ReconciliationKey) {
        self.units
            .lock()
            .expect("unit table lock")
            .insert(handle.clone(), record);
        self.keys
            .lock()
            .expect("key table lock")
            .insert(key.clone(), handle);
    }
    /// Scans durable state for a key label: live containers first,
    /// then unit files (crash-after-install leaves a file with no
    /// container).
    ///
    /// Fail-closed: spawn/query/read failures refuse with a typed
    /// error instead of reporting absence. A missing unit directory
    /// is clean absence (no units ever installed), not failure.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on podman or filesystem
    /// query failure.
    fn scan_locate(&self, key: &ReconciliationKey) -> Result<Option<UnitHandle>> {
        let out = Command::new("podman")
            .args([
                "ps",
                "--all",
                "--filter",
                &format!("label={}={}", LABEL_RECONCILIATION_KEY, key.as_str()),
                "--format",
                "{{.Names}} {{index .Config.Labels \"cistella.id\"}}",
            ])
            .output()
            .map_err(|e| CistellaError::Runtime(format!("podman ps for key: {e}")))?;
        if !out.status.success() {
            return Err(CistellaError::Runtime(format!(
                "podman ps for key failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some((name, sid)) = find_key_in_ps_output(&text) {
            return Ok(Some(self.adopt_key(key, &name, &sid)));
        }
        let Some(dir) = quadlet_dir() else {
            return Ok(None);
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(CistellaError::Runtime(format!(
                    "scan {}: {e}",
                    dir.display()
                )));
            }
        };
        if let Some((name, sid)) = scan_unit_dir(entries, key.as_str())? {
            return Ok(Some(self.adopt_key(key, &name, &sid)));
        }
        Ok(None)
    }

    /// Adopts a scan hit and binds it to the key.
    fn adopt_key(&self, key: &ReconciliationKey, name: &str, sid: &str) -> UnitHandle {
        let handle = self.adopt(name, sid);
        self.keys
            .lock()
            .expect("key table lock")
            .insert(key.clone(), handle.clone());
        handle
    }
}

/// Parses `podman ps` name/id lines for the first entry with a real
/// session id (podman's `<no value>` missing marker never matches).
#[must_use]
pub fn find_key_in_ps_output(text: &str) -> Option<(String, String)> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(sid)) = (parts.next(), parts.next())
            && !sid.starts_with('<')
        {
            return Some((name.to_string(), sid.to_string()));
        }
    }
    None
}

/// Scans one unit directory for a key label.
///
/// A `cistella-*.container` file that cannot be read refuses the
/// whole scan: an unreadable candidate could be the sought unit
/// (installed before a crash), and skipping it would report clean
/// absence into a duplicate install. Readable files lacking the key
/// skip normally; non-unit files never read.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on unreadable candidate units.
pub fn scan_unit_dir(entries: std::fs::ReadDir, key: &str) -> Result<Option<(String, String)>> {
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "container") {
            continue;
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.starts_with("cistella-") {
            continue;
        }
        let key_hit = unit_file_label(&path, LABEL_RECONCILIATION_KEY).map_err(|e| {
            CistellaError::Runtime(format!("unreadable candidate unit {}: {e}", path.display()))
        })?;
        if key_hit.as_deref() != Some(key) {
            continue;
        }
        let sid = unit_file_label(&path, LABEL_ID)
            .map_err(|e| {
                CistellaError::Runtime(format!("unreadable candidate unit {}: {e}", path.display()))
            })?
            .unwrap_or_default();
        return Ok(Some((name, sid)));
    }
    Ok(None)
}

impl Default for PodmanIsolator {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PodmanIsolator {
    fn drop(&mut self) {
        // Owner-death cleanup, not cancellation: a dropped backend
        // must not leave harness processes running past its owner.
        // Protocol cancel detaches (records survive for re-attach);
        // Drop kills what no owner remains to terminate. Best-effort
        // and nonblocking; explicit `terminate` is the reporting path.
        let mut executions = self.executions.lock().expect("exec table lock");
        for (_, mut pending) in executions.drain() {
            if let Some(mut child) = pending.child.take() {
                let _ = child.kill();
                let _ = child.try_wait();
            }
        }
    }
}

/// Classifies lifecycle state from backend observations (pure).
///
/// `active` is the systemd `ActiveState`; `container` is the podman
/// container status (`running`, `exited`, ...) or `None` when no
/// container exists; `unit_present` is unit-file presence;
/// `executing` is whether this backend currently awaits a harness on
/// the unit. Awaiting executions dominate: a running container with
/// a live await is `Executing`, without one `Initiated`.
#[must_use]
pub fn classify_state(
    active: Option<&str>,
    container: Option<&str>,
    unit_present: bool,
    executing: bool,
) -> LifecycleState {
    match container {
        Some("running") => {
            if executing {
                LifecycleState::Executing
            } else {
                LifecycleState::Initiated
            }
        }
        Some(_) => LifecycleState::Stopped,
        None => {
            if !unit_present {
                LifecycleState::Absent
            } else if active == Some("failed") {
                LifecycleState::Stopped
            } else {
                LifecycleState::Created
            }
        }
    }
}

impl Isolator for PodmanIsolator {
    fn capabilities(&self) -> IsolatorCapabilities {
        IsolatorCapabilities {
            backend: "podman-quadlet",
            supports: vec![Capability::Environment, Capability::Mounts],
        }
    }

    fn create(&self, spec: &CreateSpec, key: &ReconciliationKey) -> Result<UnitHandle> {
        // Same-key retry replays instead of duplicating: a timed-out
        // create whose unit survived (durable key label) converges on
        // the existing resource rather than installing a second one.
        // A failed scan refuses (fail-closed) instead of installing
        // blind into an uncertain outcome.
        if let Some(existing) = self.locate(key)? {
            return Ok(existing);
        }
        let session = &spec.session;
        let container_name = session.container_name();
        let unit_name = session.quadlet_unit_name();
        // The key label bakes into the unit BEFORE install: a crash
        // between install and registration still leaves a scannable
        // binding for the replacement peer.
        let unit = generate_quadlet_unit(
            session,
            &spec.volumes,
            &spec.env,
            &spec.labels,
            Some(key.as_str()),
        )?;
        // Scratch first, unit file second: any failure cleans what it
        // made, so create leaves no partial unit behind.
        let scratch_path = scratch_dir(&session.id);
        std::fs::create_dir_all(&scratch_path)
            .map_err(|e| CistellaError::Runtime(format!("create scratch: {e}")))?;
        if let Err(error) = install_quadlet(&unit_name, &unit) {
            let _ = remove_scratch(&session.id);
            return Err(error);
        }
        let handle = UnitHandle::mint();
        self.register(
            handle.clone(),
            UnitRecord {
                container_name,
                session_id: session.id.clone(),
                unit_name,
            },
            key,
        );
        Ok(handle)
    }

    fn initiate(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> Result<StartedAttestation> {
        let record = self.record(handle)?;
        start_quadlet(&record.unit_name)?;
        Ok(StartedAttestation {
            unit_identity: record.container_name,
            ready: true,
        })
    }

    fn execute_launch(
        &self,
        handle: &UnitHandle,
        argv: &[String],
        workdir: Option<&str>,
        stdio: StdioBinding,
        _key: &ReconciliationKey,
    ) -> Result<ExecutionHandle> {
        let record = self.record(handle)?;
        if !matches!(stdio, StdioBinding::Inherit) {
            return Err(CistellaError::Contract(
                "podman backend supports inherited stdio only".to_string(),
            ));
        }
        let args = match workdir {
            Some(target) => {
                crate::transport::exec_harness_args(&record.container_name, target, argv)
            }
            None => crate::transport::exec_args(&record.container_name, argv),
        };
        let child = unsafe {
            Command::new("podman")
                .args(&args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .pre_exec(|| {
                    // The child resets to default so it still dies with the pane.
                    let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
                    let _ = sigaction(Signal::SIGHUP, &dfl);
                    let _ = sigaction(Signal::SIGTERM, &dfl);
                    Ok(())
                })
                .spawn()
        }
        .map_err(|e| CistellaError::Runtime(format!("podman exec: {e}")))?;
        let execution = ExecutionHandle::mint();
        self.executions.lock().expect("exec table lock").insert(
            execution.clone(),
            PendingExec {
                unit: handle.clone(),
                child: Some(child),
                outcome: None,
            },
        );
        Ok(execution)
    }

    fn await_result(
        &self,
        execution: &ExecutionHandle,
        cancel: &CancelFlag,
    ) -> Result<ExecutionOutcome> {
        // Never hold the table lock across a wait: try_wait is
        // nonblocking, so each iteration borrows briefly. Records
        // (and collected outcomes) live until `remove`: cancelling
        // detaches without killing, and a later await re-attaches or
        // replays. Explicit `terminate` owns the kill.
        loop {
            let completed = {
                let mut table = self.executions.lock().expect("exec table lock");
                let pending = table.get_mut(execution).ok_or_else(|| {
                    CistellaError::Contract(format!(
                        "unknown execution handle: {}",
                        execution.as_str()
                    ))
                })?;
                if let Some(outcome) = pending.outcome {
                    return Ok(outcome);
                }
                let child = pending.child.as_mut().ok_or_else(|| {
                    CistellaError::Contract(format!(
                        "execution already reaped: {}",
                        execution.as_str()
                    ))
                })?;
                match child.try_wait() {
                    Ok(Some(status)) => Some(status),
                    Ok(None) => None,
                    Err(_) => {
                        // Reaped elsewhere; fall back to a blocking wait.
                        let status = child
                            .wait()
                            .map_err(|e| CistellaError::Runtime(format!("wait: {e}")))?;
                        Some(status)
                    }
                }
            };
            if let Some(status) = completed {
                let outcome = status_outcome(status, cancel);
                if let Some(pending) = self
                    .executions
                    .lock()
                    .expect("exec table lock")
                    .get_mut(execution)
                {
                    pending.outcome = Some(outcome);
                    pending.child = None;
                }
                return Ok(outcome);
            }
            if cancel.is_cancelled() || cancel_signum(cancel).is_some() {
                return Err(CistellaError::Detached(format!(
                    "await detached; execution {} stays redeemable until remove",
                    execution.as_str()
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn inspect(&self, handle: &UnitHandle) -> Result<UnitSnapshot> {
        let record = self.record(handle)?;
        let (container, session_id, image) = inspect_container(&record.container_name)?;
        let props = query_unit_props(&format!("{}.service", record.container_name));
        let active_state = props
            .get("ActiveState")
            .cloned()
            .unwrap_or_else(|| "inactive".to_string());
        let unit_present = unit_file_present(&record.unit_name);
        // Executing means an unreaped child exists, not merely a
        // retained record (outcomes replay after completion).
        let executing = self
            .executions
            .lock()
            .expect("exec table lock")
            .values()
            .any(|pending| pending.child.is_some());
        let lifecycle = classify_state(
            Some(&active_state),
            container.as_deref(),
            unit_present,
            !executing,
        );
        Ok(UnitSnapshot {
            unit_identity: record.container_name,
            lifecycle,
            active_state,
            session_id,
            image,
        })
    }

    fn state(&self, handle: &UnitHandle) -> Result<LifecycleState> {
        Ok(self.inspect(handle)?.lifecycle)
    }

    fn terminate(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        _key: &ReconciliationKey,
    ) -> Result<StoppedAttestation> {
        let record = self.record(handle)?;
        // Absent units succeed: a missing service with no container is
        // already stopped, which is the converged state.
        if !unit_file_present(&record.unit_name) && !container_exists(&record.container_name)? {
            return Ok(StoppedAttestation {
                unit_identity: record.container_name,
            });
        }
        stop_settle(&record.container_name, grace)?;
        Ok(StoppedAttestation {
            unit_identity: record.container_name,
        })
    }

    fn remove(&self, handle: &UnitHandle, _key: &ReconciliationKey) -> Result<RemovedAttestation> {
        let record = self.record(handle)?;
        remove_unit_file(&record.container_name, &record.unit_name)?;
        if !record.session_id.is_empty() {
            remove_scratch(&record.session_id)?;
        }
        // Execution records die with the unit: outcomes stay
        // replayable until `remove`, never after.
        self.executions
            .lock()
            .expect("exec table lock")
            .retain(|_, pending| pending.unit != *handle);
        Ok(RemovedAttestation {
            unit_identity: record.container_name,
        })
    }

    fn locate(&self, key: &ReconciliationKey) -> Result<Option<UnitHandle>> {
        // Memory hit is a hint, not proof: verify against durable
        // state before returning, or a stale entry could mask a
        // reaped unit and greenlight a duplicate install.
        if let Some(handle) = self.keys.lock().expect("key table lock").get(key).cloned()
            && let Ok(record) = self.record(&handle)
        {
            let alive = container_exists(&record.container_name).unwrap_or(false)
                || unit_file_present(&record.unit_name);
            if alive {
                return Ok(Some(handle));
            }
            self.keys.lock().expect("key table lock").remove(key);
        }
        self.scan_locate(key)
    }

    fn converge_clean(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        key: &ReconciliationKey,
    ) -> Result<()> {
        // Never trust `state` alone: an `Absent` unit with surviving
        // scratch is residue, not convergence. `remove` is idempotent
        // and owns both the unit file and scratch, so it closes every
        // state including absent-with-residue.
        match self.state(handle) {
            Ok(LifecycleState::Absent) => {
                self.remove(handle, key)?;
                Ok(())
            }
            Ok(_) => {
                self.terminate(handle, grace, key)?;
                self.remove(handle, key)?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// Converts a waited child status, preferring a pending cancellation
/// disposition over the raw exit code (conduct-level signals dominate).
fn status_outcome(status: std::process::ExitStatus, cancel: &CancelFlag) -> ExecutionOutcome {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signum) = cancel.signum() {
        return ExecutionOutcome::Signaled(signum);
    }
    if signals::got_hup() {
        return ExecutionOutcome::Signaled(1);
    }
    if signals::got_term() {
        return ExecutionOutcome::Signaled(15);
    }
    if let Some(signal) = status.signal() {
        ExecutionOutcome::Signaled(signal)
    } else {
        ExecutionOutcome::Exited(status.code().unwrap_or(1))
    }
}

/// Pending cancellation signal, if any.
fn cancel_signum(cancel: &CancelFlag) -> Option<i32> {
    if signals::got_hup() {
        return Some(1);
    }
    if signals::got_term() {
        return Some(15);
    }
    cancel.signum()
}

/// Kills a harness child: SIGTERM, brief grace, SIGKILL on survival.
/// Inspects a container: status, `cistella.id` label, image.
///
/// Absence (`podman container exists` exit 1) returns `None` status
/// with empty strings; any other failure is a typed error (a failed
/// inspect never reads as absence).
fn inspect_container(name: &str) -> Result<(Option<String>, String, String)> {
    let out = Command::new("podman")
        .args([
            "inspect",
            "--format",
            "{{.State.Status}} {{index .Config.Labels \"cistella.id\"}} {{.ImageName}}",
            name,
        ])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman inspect: {e}")))?;
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let mut parts = text.split_whitespace();
        let status = parts.next().map(|s| s.to_string());
        let sid = match parts.next() {
            Some(id) if !id.starts_with('<') => id.to_string(),
            _ => String::new(),
        };
        let image = parts.next().unwrap_or_default().to_string();
        return Ok((status, sid, image));
    }
    if container_exists(name)? {
        return Err(CistellaError::Runtime(format!(
            "podman inspect {name} failed while container exists: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok((None, String::new(), String::new()))
}

/// Reports Quadlet unit-file presence.
fn unit_file_present(unit_name: &str) -> bool {
    quadlet_dir()
        .map(|dir| dir.join(unit_name).exists())
        .unwrap_or(false)
}
