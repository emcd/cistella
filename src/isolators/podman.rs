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
struct PendingExec {
    /// Running harness child.
    child: Child,
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
}

impl Default for PodmanIsolator {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PodmanIsolator {
    fn drop(&mut self) {
        // Best-effort reap of never-awaited executions: kill and
        // release without blocking the drop.
        let mut executions = self.executions.lock().expect("exec table lock");
        for (_, mut pending) in executions.drain() {
            let _ = pending.child.kill();
            let _ = pending.child.try_wait();
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
        let session = &spec.session;
        let container_name = session.container_name();
        let unit_name = session.quadlet_unit_name();
        let unit = generate_quadlet_unit(session, &spec.volumes, &spec.env, &spec.labels)?;
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
        self.executions
            .lock()
            .expect("exec table lock")
            .insert(execution.clone(), PendingExec { child });
        Ok(execution)
    }

    fn await_result(
        &self,
        execution: &ExecutionHandle,
        cancel: &CancelFlag,
    ) -> Result<ExecutionOutcome> {
        let mut pending = self
            .executions
            .lock()
            .expect("exec table lock")
            .remove(execution)
            .ok_or_else(|| {
                CistellaError::Contract(format!("unknown execution handle: {}", execution.as_str()))
            })?;
        let child = &mut pending.child;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status_outcome(status, cancel)),
                Ok(None) => {}
                Err(_) => {
                    // Reaped elsewhere; fall back to a blocking wait.
                    let status = child
                        .wait()
                        .map_err(|e| CistellaError::Runtime(format!("wait: {e}")))?;
                    return Ok(status_outcome(status, cancel));
                }
            }
            if let Some(signum) = cancel_signum(cancel) {
                kill_child(child);
                return Ok(ExecutionOutcome::Signaled(signum));
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
        let executing = self.executions.lock().expect("exec table lock").is_empty();
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
        Ok(RemovedAttestation {
            unit_identity: record.container_name,
        })
    }

    fn locate(&self, key: &ReconciliationKey) -> Option<UnitHandle> {
        self.keys.lock().expect("key table lock").get(key).cloned()
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
        ExecutionOutcome::Signaled(128 + signal)
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
fn kill_child(child: &mut Child) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let pid = Pid::from_raw(child.id() as i32);
    let _ = kill(pid, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(200));
    if child.try_wait().unwrap_or(None).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

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
