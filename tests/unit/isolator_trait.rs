//! Isolator trait conformance (fast half): state classification,
//! cancellation, and the default `converge_clean` against an
//! in-memory fake. The live half (Podman lifecycle fidelity) runs in
//! `tests/integration/conformance.rs`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use cistella::framework::contract::{
    CancelFlag, Capability, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use cistella::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use cistella::isolators::podman::classify_state;

/// In-memory fake: units flip through states without any runtime.
///
/// Proves trait-level logic only (converge ordering, idempotent
/// teardown, cancellation plumbing). It is NOT the deterministic
/// protocol peer (task 3.1): no framing, no faults, no lifecycle
/// meaning beyond the state machine the trait requires.
struct MemIsolator {
    states: Mutex<HashMap<String, LifecycleState>>,
    terminated: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
}

impl MemIsolator {
    fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            terminated: Mutex::new(Vec::new()),
            removed: Mutex::new(Vec::new()),
        }
    }

    fn key(handle: &UnitHandle) -> String {
        handle.as_str().to_string()
    }
}

impl Isolator for MemIsolator {
    fn capabilities(&self) -> IsolatorCapabilities {
        IsolatorCapabilities {
            backend: "mem-fake",
            supports: vec![Capability::Environment, Capability::Mounts],
        }
    }

    fn create(
        &self,
        _spec: &CreateSpec,
        _key: &ReconciliationKey,
    ) -> Result<UnitHandle, cistella::error::CistellaError> {
        let handle = UnitHandle::mint();
        self.states
            .lock()
            .unwrap()
            .insert(Self::key(&handle), LifecycleState::Created);
        Ok(handle)
    }

    fn initiate(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> Result<StartedAttestation, cistella::error::CistellaError> {
        self.states
            .lock()
            .unwrap()
            .insert(Self::key(handle), LifecycleState::Initiated);
        Ok(StartedAttestation {
            unit_identity: Self::key(handle),
            ready: true,
        })
    }

    fn execute_launch(
        &self,
        _handle: &UnitHandle,
        _argv: &[String],
        _workdir: Option<&str>,
        _stdio: StdioBinding,
        _key: &ReconciliationKey,
    ) -> Result<ExecutionHandle, cistella::error::CistellaError> {
        Ok(ExecutionHandle::mint())
    }

    fn await_result(
        &self,
        _execution: &ExecutionHandle,
        cancel: &CancelFlag,
    ) -> Result<ExecutionOutcome, cistella::error::CistellaError> {
        if cancel.is_cancelled() {
            return Ok(ExecutionOutcome::Signaled(cancel.signum().unwrap_or(15)));
        }
        Ok(ExecutionOutcome::Exited(0))
    }

    fn inspect(&self, handle: &UnitHandle) -> Result<UnitSnapshot, cistella::error::CistellaError> {
        let lifecycle = self
            .states
            .lock()
            .unwrap()
            .get(&Self::key(handle))
            .copied()
            .unwrap_or(LifecycleState::Absent);
        Ok(UnitSnapshot {
            unit_identity: Self::key(handle),
            lifecycle,
            active_state: "active".to_string(),
            session_id: String::new(),
            image: String::new(),
        })
    }

    fn state(&self, handle: &UnitHandle) -> Result<LifecycleState, cistella::error::CistellaError> {
        Ok(self.inspect(handle)?.lifecycle)
    }

    fn terminate(
        &self,
        handle: &UnitHandle,
        _grace: Duration,
        _key: &ReconciliationKey,
    ) -> Result<StoppedAttestation, cistella::error::CistellaError> {
        self.terminated.lock().unwrap().push(Self::key(handle));
        self.states
            .lock()
            .unwrap()
            .insert(Self::key(handle), LifecycleState::Stopped);
        Ok(StoppedAttestation {
            unit_identity: Self::key(handle),
        })
    }

    fn remove(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> Result<RemovedAttestation, cistella::error::CistellaError> {
        self.removed.lock().unwrap().push(Self::key(handle));
        self.states
            .lock()
            .unwrap()
            .insert(Self::key(handle), LifecycleState::Absent);
        Ok(RemovedAttestation {
            unit_identity: Self::key(handle),
        })
    }

    fn locate(&self, _key: &ReconciliationKey) -> Option<UnitHandle> {
        None
    }
}

#[test]
fn converge_clean_terminates_then_removes() {
    let backend = MemIsolator::new();
    let key = ReconciliationKey::generate();
    let grace = Duration::from_secs(1);
    // Absent converges without touching the backend.
    let missing = UnitHandle::mint();
    backend.converge_clean(&missing, grace, &key).unwrap();
    assert!(backend.terminated.lock().unwrap().is_empty());
    assert!(backend.removed.lock().unwrap().is_empty());
    // Present converges through terminate then remove, in that order.
    backend
        .states
        .lock()
        .unwrap()
        .insert(MemIsolator::key(&missing), LifecycleState::Initiated);
    backend.converge_clean(&missing, grace, &key).unwrap();
    assert_eq!(backend.terminated.lock().unwrap().len(), 1);
    assert_eq!(backend.removed.lock().unwrap().len(), 1);
    assert_eq!(backend.state(&missing).unwrap(), LifecycleState::Absent);
}

#[test]
fn await_honors_cancellation() {
    let backend = MemIsolator::new();
    let cancel = CancelFlag::default();
    let execution = ExecutionHandle::mint();
    assert_eq!(
        backend.await_result(&execution, &cancel).unwrap(),
        ExecutionOutcome::Exited(0)
    );
    cancel.cancel_with(15);
    assert_eq!(
        backend.await_result(&execution, &cancel).unwrap(),
        ExecutionOutcome::Signaled(15)
    );
}

#[test]
fn outcome_exit_codes_follow_disposition() {
    assert_eq!(ExecutionOutcome::Exited(3).exit_code(), 3);
    assert_eq!(ExecutionOutcome::Signaled(15).exit_code(), 143);
}

#[test]
fn classify_state_matrix() {
    // Running with a live await executes; without one it initiated.
    assert_eq!(
        classify_state(Some("active"), Some("running"), true, true),
        LifecycleState::Executing
    );
    assert_eq!(
        classify_state(Some("active"), Some("running"), true, false),
        LifecycleState::Initiated
    );
    // Any non-running container status is stopped.
    assert_eq!(
        classify_state(Some("active"), Some("exited"), true, false),
        LifecycleState::Stopped
    );
    assert_eq!(
        classify_state(Some("failed"), Some("exited"), true, false),
        LifecycleState::Stopped
    );
    // No container and no unit file is absent.
    assert_eq!(
        classify_state(None, None, false, false),
        LifecycleState::Absent
    );
    // No container with a failed unit is stopped; otherwise created.
    assert_eq!(
        classify_state(Some("failed"), None, true, false),
        LifecycleState::Stopped
    );
    assert_eq!(
        classify_state(Some("inactive"), None, true, false),
        LifecycleState::Created
    );
}

#[test]
fn capabilities_gate_matches_contract() {
    let backend = MemIsolator::new();
    let capabilities = backend.capabilities();
    assert!(capabilities.supports(Capability::Environment));
    assert!(capabilities.supports(Capability::Mounts));
    assert!(!capabilities.supports(Capability::GuestHooks));
}
