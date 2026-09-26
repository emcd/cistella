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
/// teardown, detach/replay plumbing). It is NOT the deterministic
/// protocol peer (task 3.1): no framing, no faults, no lifecycle
/// meaning beyond the state machine the trait requires.
struct MemIsolator {
    states: Mutex<HashMap<String, LifecycleState>>,
    terminated: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
    outcomes: Mutex<HashMap<String, ExecutionOutcome>>,
}

impl MemIsolator {
    fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            terminated: Mutex::new(Vec::new()),
            removed: Mutex::new(Vec::new()),
            outcomes: Mutex::new(HashMap::new()),
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
            pidns_proof: "pid:[fake]".to_string(),
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
        execution: &ExecutionHandle,
        cancel: &CancelFlag,
    ) -> Result<ExecutionOutcome, cistella::error::CistellaError> {
        // Detach without killing: cancellation leaves the record for
        // re-attach; completion stores the outcome for replay. A
        // completed outcome replays even when cancelled.
        let mut outcomes = self.outcomes.lock().unwrap();
        if let Some(outcome) = outcomes.get(execution.as_str()) {
            return Ok(*outcome);
        }
        if cancel.is_cancelled() {
            return Err(cistella::error::CistellaError::Detached(format!(
                "detached: {}",
                execution.as_str()
            )));
        }
        outcomes.insert(execution.as_str().to_string(), ExecutionOutcome::Exited(0));
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

    fn locate(
        &self,
        _key: &ReconciliationKey,
    ) -> Result<Option<UnitHandle>, cistella::error::CistellaError> {
        Ok(None)
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
fn await_detaches_without_killing_and_replays() {
    let backend = MemIsolator::new();
    let live = CancelFlag::default();
    let execution = ExecutionHandle::mint();
    // Completion stores the outcome.
    assert_eq!(
        backend.await_result(&execution, &live).unwrap(),
        ExecutionOutcome::Exited(0)
    );
    // Replay returns the stored outcome without re-running.
    assert_eq!(
        backend.await_result(&execution, &live).unwrap(),
        ExecutionOutcome::Exited(0)
    );
    // Cancellation on a live execution detaches with a typed
    // error; the record survives for re-attach.
    let fresh = ExecutionHandle::mint();
    let cancel = CancelFlag::default();
    cancel.cancel_with(15);
    let error = backend.await_result(&fresh, &cancel).unwrap_err();
    assert!(error.to_string().contains("detached"));
    // Re-attach after detach completes and replays.
    assert_eq!(
        backend.await_result(&fresh, &live).unwrap(),
        ExecutionOutcome::Exited(0)
    );
    assert_eq!(
        backend.await_result(&fresh, &live).unwrap(),
        ExecutionOutcome::Exited(0)
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

#[test]
fn ps_output_parsing_skips_missing_markers() {
    use cistella::isolators::podman::find_key_in_ps_output;
    assert_eq!(
        find_key_in_ps_output("cistella-abc abc123\n"),
        Some(("cistella-abc".to_string(), "abc123".to_string()))
    );
    // Podman's `<no value>` marker never matches.
    assert_eq!(find_key_in_ps_output("cistella-abc <no value>\n"), None);
    assert_eq!(find_key_in_ps_output(""), None);
}

#[test]
fn unit_dir_scan_finds_key_and_ignores_others() {
    use cistella::isolators::podman::scan_unit_dir;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("cistella-abc.container"),
        "[Container]\nLabel=cistella.reconciliation-key=\"key-1\"\nLabel=cistella.id=\"abc\"\n",
    )
    .expect("write unit");
    std::fs::write(
        dir.path().join("cistella-other.container"),
        "[Container]\nLabel=cistella.reconciliation-key=\"key-2\"\n",
    )
    .expect("write unit");
    std::fs::write(dir.path().join("notes.txt"), "not a unit").expect("write notes");
    let entries = std::fs::read_dir(dir.path()).expect("read dir");
    assert_eq!(
        scan_unit_dir(entries, "key-1").expect("scan runs clean"),
        Some(("cistella-abc".to_string(), "abc".to_string()))
    );
    let entries = std::fs::read_dir(dir.path()).expect("read dir");
    assert_eq!(
        scan_unit_dir(entries, "key-9").expect("scan runs clean"),
        None
    );
}

#[test]
fn unit_dir_scan_refuses_unreadable_candidates() {
    use cistella::isolators::podman::scan_unit_dir;
    let dir = tempfile::tempdir().expect("tempdir");
    let locked = dir.path().join("cistella-locked.container");
    std::fs::write(&locked, "[Container]\nLabel=cistella.id=\"x\"\n").expect("write unit");
    // Owner bits fully cleared (read-only alone stays readable).
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if std::fs::read(&locked).is_ok() {
        // File modes not enforced here (e.g. running as root): the
        // refusal path cannot be exercised.
        eprintln!("skip: unreadable-file probe reads back");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).expect("restore");
        return;
    }
    let entries = std::fs::read_dir(dir.path()).expect("read dir");
    let error = scan_unit_dir(entries, "key-1").unwrap_err();
    assert!(error.to_string().contains("unreadable candidate unit"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).expect("restore");
}

#[test]
fn unit_dir_scan_refuses_failed_iteration() {
    use cistella::isolators::podman::scan_unit_dir;
    // Deterministic iterator failure: no filesystem stages a
    // mid-iteration getdents error reliably (open fds keep working
    // past chmod), so the test injects the Err item directly.
    let injected: std::io::Result<std::fs::DirEntry> = Err(std::io::Error::other("staged"));
    let error = scan_unit_dir(vec![injected].into_iter(), "key-1").unwrap_err();
    assert!(error.to_string().contains("unit directory iteration"));
}

#[test]
fn foreground_join_truth_table() {
    // Terminal plus verified conductor joins; terminal without
    // conductor refuses typed (a silent background launch would
    // stall with SIGTTIN); piped never moves. In-process Inherit
    // never consults this (already grouped).
    use cistella::framework::isolator::{ForegroundJoin, foreground_join};
    assert_eq!(
        foreground_join(true, Some((5, 7))),
        ForegroundJoin::Join((5, 7))
    );
    assert_eq!(foreground_join(true, None), ForegroundJoin::Refuse);
    assert_eq!(foreground_join(false, Some((5, 7))), ForegroundJoin::Stay);
    assert_eq!(foreground_join(false, None), ForegroundJoin::Stay);
}

#[test]
fn verified_conductor_pgid_truth_table() {
    // Parentage equality authenticates (a subreaper adoption fails
    // even when it is not init); only then does a looked-up pgid
    // pass through, and a failed lookup maps to None for the TTY
    // branch to refuse downstream.
    use cistella::framework::isolator::verified_conductor_pgid;
    assert_eq!(verified_conductor_pgid(5, 5, Some(9)), Some((5, 9)));
    assert_eq!(verified_conductor_pgid(5, 5, None), None);
    assert_eq!(verified_conductor_pgid(5, 6, Some(9)), None);
    assert_eq!(verified_conductor_pgid(5, 6, None), None);
    assert_eq!(verified_conductor_pgid(5, 1, Some(9)), None);
}
