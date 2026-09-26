//! Scripted isolator backend shared by wire-dispatch tests.
//!
//! Split from `wire_dispatch.rs` at the file-size limit. In-memory
//! units plus a launch log, so replay/tombstone proofs run with no
//! Podman. Shared across guest instances through clone to prove
//! adopt-after-restart convergence.

use cistella::framework::contract::{LifecycleState, ReconciliationKey, UnitHandle};
use cistella::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use cistella::session::Session;

/// Scripted backend: in-memory units plus a launch log, so replay
/// proofs run with no Podman. Shared across guest instances through
/// clone to prove adopt-after-restart convergence.
#[derive(Debug, Clone, Default)]
pub struct FakeBackend {
    /// Local handle to lifecycle state.
    pub units:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<UnitHandle, LifecycleState>>>,
    /// Reconciliation key to local handle.
    pub keys:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<ReconciliationKey, UnitHandle>>>,
    /// Launched argv per call (double-spawn detector).
    pub launches: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    /// Backend `create` call count (adopt must not recreate).
    pub creates: std::sync::Arc<std::sync::Mutex<usize>>,
    /// Terminated unit handles, in order.
    pub terminated: std::sync::Arc<std::sync::Mutex<Vec<UnitHandle>>>,
    /// Removed unit handles, in order.
    pub removed: std::sync::Arc<std::sync::Mutex<Vec<UnitHandle>>>,
    /// Local handle to session id (scratch ownership for the
    /// tombstone scratch model below).
    pub sessions: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<UnitHandle, String>>>,
    /// Session ids with surviving scratch residue (planted by
    /// tests, cleared by remove — the absent-scratch model).
    pub scratch: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl Isolator for FakeBackend {
    fn capabilities(&self) -> IsolatorCapabilities {
        IsolatorCapabilities {
            backend: "fake",
            supports: vec![],
        }
    }

    fn create(
        &self,
        spec: &CreateSpec,
        key: &ReconciliationKey,
    ) -> cistella::error::Result<UnitHandle> {
        let local = UnitHandle::mint();
        self.units
            .lock()
            .unwrap()
            .insert(local.clone(), LifecycleState::Created);
        self.keys.lock().unwrap().insert(key.clone(), local.clone());
        self.sessions
            .lock()
            .unwrap()
            .insert(local.clone(), spec.session.id.clone());
        *self.creates.lock().unwrap() += 1;
        Ok(local)
    }

    fn initiate(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<StartedAttestation> {
        let mut units = self.units.lock().unwrap();
        let state = units.get_mut(handle).ok_or_else(|| {
            cistella::error::CistellaError::Contract("fake: unknown unit".to_string())
        })?;
        *state = LifecycleState::Initiated;
        Ok(StartedAttestation {
            unit_identity: handle.as_str().to_string(),
            pidns_proof: "pid:[fake]".to_string(),
            ready: true,
        })
    }

    fn execute_launch(
        &self,
        handle: &UnitHandle,
        argv: &[String],
        _workdir: Option<&str>,
        _stdio: StdioBinding,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<cistella::framework::contract::ExecutionHandle> {
        if !self.units.lock().unwrap().contains_key(handle) {
            return Err(cistella::error::CistellaError::Contract(
                "fake: unknown unit".to_string(),
            ));
        }
        self.launches.lock().unwrap().push(argv.to_vec());
        Ok(cistella::framework::contract::ExecutionHandle::mint())
    }

    fn await_result(
        &self,
        _execution: &cistella::framework::contract::ExecutionHandle,
        _cancel: &cistella::framework::contract::CancelFlag,
    ) -> cistella::error::Result<ExecutionOutcome> {
        Ok(ExecutionOutcome::Exited(0))
    }

    fn inspect(&self, handle: &UnitHandle) -> cistella::error::Result<UnitSnapshot> {
        let units = self.units.lock().unwrap();
        let lifecycle = units.get(handle).copied().unwrap_or(LifecycleState::Absent);
        Ok(UnitSnapshot {
            unit_identity: handle.as_str().to_string(),
            lifecycle,
            active_state: "active".to_string(),
            session_id: String::new(),
            image: String::new(),
        })
    }

    fn state(&self, handle: &UnitHandle) -> cistella::error::Result<LifecycleState> {
        Ok(self
            .units
            .lock()
            .unwrap()
            .get(handle)
            .copied()
            .unwrap_or(LifecycleState::Absent))
    }

    fn terminate(
        &self,
        handle: &UnitHandle,
        _grace: std::time::Duration,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<StoppedAttestation> {
        self.terminated.lock().unwrap().push(handle.clone());
        Ok(StoppedAttestation {
            unit_identity: handle.as_str().to_string(),
        })
    }

    fn remove(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<RemovedAttestation> {
        self.removed.lock().unwrap().push(handle.clone());
        // Scratch model: removing clears the unit's surviving
        // residue, so a planted scratch plus a re-remove proves
        // tombstone delegation end to end.
        if let Some(session_id) = self.sessions.lock().unwrap().get(handle) {
            self.scratch.lock().unwrap().remove(session_id);
        }
        Ok(RemovedAttestation {
            unit_identity: handle.as_str().to_string(),
        })
    }

    fn locate(&self, key: &ReconciliationKey) -> cistella::error::Result<Option<UnitHandle>> {
        Ok(self.keys.lock().unwrap().get(key).cloned())
    }
}

/// Minimal creation spec for replay payloads.
pub fn replay_spec() -> CreateSpec {
    CreateSpec {
        session: Session {
            id: "replayprobe".to_string(),
            directory: "/tmp/replayprobe".to_string(),
            profile: "replay".to_string(),
            profile_digest: "digest".to_string(),
            identity: "tester".to_string(),
            command: vec!["true".to_string()],
            image: "localhost/cistella/opencode:example".to_string(),
            container_home: "/home/cistella".to_string(),
        },
        volumes: vec![],
        env: vec![],
        labels: vec![],
    }
}

pub fn create_payload(fw: &str, key: &ReconciliationKey, spec: &CreateSpec) -> serde_json::Value {
    serde_json::json!({
        "unit_handle": fw,
        "spec": serde_json::to_value(spec).expect("spec serializes"),
        "reconciliation_key": key.as_str(),
    })
}

pub fn launch_payload(
    unit: &str,
    exec: &str,
    key: &ReconciliationKey,
    argv: &[&str],
) -> serde_json::Value {
    // No stdio in the payload: harness descriptors arrive over the
    // fd channel in a staged bundle.
    serde_json::json!({
        "unit_handle": unit,
        "execution_handle": exec,
        "argv": argv,
        "workdir": null,
        "reconciliation_key": key.as_str(),
    })
}
