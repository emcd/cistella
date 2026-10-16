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

#[path = "podman_isolator.rs"]
mod podman_isolator;

pub(crate) use super::{hook, podman_await};

use std::collections::HashMap;
use std::process::{Child, Command};
use std::sync::Mutex;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle};
use crate::framework::isolator::ExecutionOutcome;
use crate::isolators::quadlet::{container_exists, quadlet_dir};
use crate::registry::unit_file_label;
use crate::session::{LABEL_ID, LABEL_RECONCILIATION_KEY};

/// Unit record tracked per issued handle.
#[derive(Debug, Clone)]
pub(crate) struct UnitRecord {
    /// Container name (unit identity).
    pub(crate) container_name: String,
    /// Session id.
    pub(crate) session_id: String,
    /// Quadlet unit file name.
    pub(crate) unit_name: String,
}

/// Handle-table entry: a live record plus, after successful
/// remove, a removal tombstone. Eviction clears the live binding
/// (no resurrection through it); the tombstone retains session
/// identity so identical convergent retries report absent
/// attestations and absent-with-unit still converges by handle.
#[derive(Debug, Clone)]
pub(crate) enum UnitEntry {
    /// Live backend unit.
    Live(UnitRecord),
    /// Removal tombstone: live binding evicted, absence recorded.
    Removed {
        /// Container name (unit identity for attestations).
        container_name: String,
        /// Session id (identity for re-converge).
        session_id: String,
        /// Quadlet unit file name (presence checks).
        unit_name: String,
    },
}

/// Pending launched execution awaiting `await_result`.
///
/// The record (and, once collected, the outcome) lives until
/// `remove`: cancelling detaches without killing, and a later await
/// on the same handle re-attaches or replays. The table retains
/// identity only — the caller owns the reconciliation key for any
/// cross-call operation (terminate/remove take it as a parameter).
pub(crate) struct PendingExec {
    /// Unit this execution belongs to (cleared with the unit).
    pub(crate) unit: UnitHandle,
    /// Running harness child (None once reaped).
    pub(crate) child: Option<Child>,
    /// Collected outcome for replay until `remove`.
    pub(crate) outcome: Option<ExecutionOutcome>,
}

/// Podman isolator backend (`podman` + Quadlet + systemd user manager).
pub struct PodmanIsolator {
    /// Issued handles to unit entries (live records plus removal
    /// tombstones).
    pub(crate) units: Mutex<HashMap<UnitHandle, UnitEntry>>,
    /// Reconciliation keys to handles (replacement-peer recovery).
    pub(crate) keys: Mutex<HashMap<ReconciliationKey, UnitHandle>>,
    /// Launched executions awaiting result collection.
    pub(crate) executions: Mutex<HashMap<ExecutionHandle, PendingExec>>,
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
            .insert(handle.clone(), UnitEntry::Live(record));
        handle
    }

    /// Looks up the entry for a handle (live record or tombstone).
    fn resolve(&self, handle: &UnitHandle) -> Result<UnitEntry> {
        self.units
            .lock()
            .expect("unit table lock")
            .get(handle)
            .cloned()
            .ok_or_else(|| {
                CistellaError::Contract(format!("unknown unit handle: {}", handle.as_str()))
            })
    }

    /// Resolves one execution to its live container/unit names for
    /// read-only corroboration. Returns `None` for unknown
    /// executions and tombstoned units (see
    /// `crate::isolators::corroboration`).
    pub(crate) fn corroboration_names(
        &self,
        execution: &ExecutionHandle,
    ) -> Option<(String, String)> {
        let executions = self.executions.lock().expect("exec table lock");
        let units = self.units.lock().expect("unit table lock");
        let pending = executions.get(execution)?;
        match units.get(&pending.unit)? {
            UnitEntry::Live(record) => {
                Some((record.container_name.clone(), record.unit_name.clone()))
            }
            UnitEntry::Removed { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn insert_test_execution(
        &self,
        unit: UnitHandle,
        execution: ExecutionHandle,
        child: std::process::Child,
        container_name: &str,
        session_id: &str,
    ) {
        let record = UnitRecord {
            container_name: container_name.to_string(),
            session_id: session_id.to_string(),
            unit_name: format!("{container_name}.container"),
        };
        self.units
            .lock()
            .expect("unit table lock")
            .insert(unit.clone(), UnitEntry::Live(record));
        self.executions.lock().expect("exec table lock").insert(
            execution,
            PendingExec {
                unit,
                child: Some(child),
                outcome: None,
            },
        );
    }

    /// Looks up the live record for a handle: tombstones refuse as
    /// removed (convergent tombstone arms answer without a record;
    /// live attempts on removed handles never re-enter the backend).
    fn record(&self, handle: &UnitHandle) -> Result<UnitRecord> {
        match self.resolve(handle)? {
            UnitEntry::Live(record) => Ok(record),
            UnitEntry::Removed { .. } => Err(CistellaError::Contract(format!(
                "unit handle removed: {}",
                handle.as_str()
            ))),
        }
    }

    /// Records a handle/key pair after successful creation.
    fn register(&self, handle: UnitHandle, record: UnitRecord, key: &ReconciliationKey) {
        self.units
            .lock()
            .expect("unit table lock")
            .insert(handle.clone(), UnitEntry::Live(record));
        self.keys
            .lock()
            .expect("key table lock")
            .insert(key.clone(), handle);
    }
    /// Scans durable state for a key label: live containers first,
    /// then unit files (crash-after-install leaves a file with no
    /// container).
    ///
    /// Template-dialect discipline: `ps --format` field selection
    /// varies by version (4.9.3 has no `.Config` on ps rows), so ps
    /// lists NAMES ONLY and every candidate is verified through
    /// `inspect` (`json` template func plus `index` on present keys
    /// is version-stable). The label filter is an optimization
    /// only: adoption requires the key label to verify, never
    /// trusts the filter output.
    ///
    /// Fail-closed: spawn/query/read failures refuse with a typed
    /// error instead of reporting absence. A missing unit directory
    /// is clean absence (no units ever installed), not failure. A
    /// candidate that vanishes mid-scan skips (its absence is real);
    /// any other query failure refuses.
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
                "{{.Names}}",
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
        for name in find_container_names(&text) {
            let Some(labels) = inspect_labels(&name)? else {
                continue;
            };
            let hit = labels
                .get(LABEL_RECONCILIATION_KEY)
                .and_then(|label| label.as_str())
                == Some(key.as_str());
            if !hit {
                continue;
            }
            let sid = labels
                .get(LABEL_ID)
                .and_then(|label| label.as_str())
                .unwrap_or_default()
                .to_string();
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

/// Parses names-only `podman ps` output into candidate container
/// names (one per non-empty line). Label verification happens per
/// candidate through `inspect_labels`, never through ps field
/// rendering.
#[must_use]
pub fn find_container_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Reads one container's config labels as a JSON map (`json`
/// template func is version-stable, unlike ps field selection).
/// Returns `None` when the container vanished mid-scan (its
/// absence is real, not failure); any other query failure refuses
/// fail-closed.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on spawn failure, inspect
/// failure for a present container, or unparsable output.
fn inspect_labels(name: &str) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let out = Command::new("podman")
        .args(["inspect", "--format", "{{json .Config.Labels}}", name])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman inspect labels: {e}")))?;
    if !out.status.success() {
        // Vanished between ps and inspect reads as absence; a
        // present-but-uninspectable container refuses (fail-closed:
        // an uncertain scan must never greenlight a duplicate).
        let gone = !container_exists(name).unwrap_or(true);
        if gone {
            return Ok(None);
        }
        return Err(CistellaError::Runtime(format!(
            "podman inspect labels {name} failed while container exists: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&out.stdout)
        .map(Some)
        .map_err(|e| CistellaError::Runtime(format!("podman inspect labels {name}: {e}")))
}

/// Scans one unit directory for a key label.
///
/// Generic over the entry iterator so failing iteration is
/// unit-testable (production callers pass `std::fs::ReadDir`
/// directly). Any `Err` item refuses the whole scan, because an
/// unreadable candidate or a skipped entry could be the sought unit
/// (installed before a crash), and either would report clean
/// absence into a duplicate install. Readable files lacking the key
/// skip normally; non-unit files never read.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on iteration failure or
/// unreadable candidate units.
pub fn scan_unit_dir(
    entries: impl Iterator<Item = std::io::Result<std::fs::DirEntry>>,
    key: &str,
) -> Result<Option<(String, String)>> {
    for entry in entries {
        let entry =
            entry.map_err(|e| CistellaError::Runtime(format!("unit directory iteration: {e}")))?;
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

/// Kills a harness child: SIGTERM, brief grace, SIGKILL on survival.
/// Proves the container's PID namespace: polls `podman inspect` for
/// a nonzero container init PID, then reads its `ns/pid` link
/// (`pid:[inode]` form). Bounds match the start poll above; an
/// unprovable namespace fails initiate rather than attesting
/// blindly (the caller tears down).
fn container_pidns(name: &str) -> Result<String> {
    use std::time::Instant;
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let out = Command::new("podman")
            .args(["inspect", "--format", "{{.State.Pid}}", name])
            .output()
            .map_err(|e| CistellaError::Runtime(format!("podman inspect pid: {e}")))?;
        if out.status.success() {
            let pid: i32 = String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0);
            if pid > 0 {
                let link = std::fs::read_link(format!("/proc/{pid}/ns/pid"))
                    .map_err(|e| CistellaError::Runtime(format!("read pidns of {pid}: {e}")))?;
                return Ok(link.to_string_lossy().to_string());
            }
        }
        if Instant::now() > deadline {
            return Err(CistellaError::Runtime(format!(
                "container {name} yielded no pidns proof after 30s"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
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
pub(crate) fn unit_file_present(unit_name: &str) -> bool {
    quadlet_dir()
        .map(|dir| dir.join(unit_name).exists())
        .unwrap_or(false)
}
