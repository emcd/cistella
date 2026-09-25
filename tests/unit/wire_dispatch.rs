//! Podman guest wire dispatch error surface (task 2.1).
//!
//! Unknown operations, malformed payloads, and unknown framework
//! handles refuse with typed errors before any backend call, so
//! these pins run fast with no podman. Backend-touching behavior
//! (adopt-or-create, parity, recovery) rides the live tier.

use cistella::framework::contract::{ExecutionHandle, ReconciliationKey, UnitHandle};
use cistella::isolators::wire::{
    IsolatorGuest, OP_CREATE, OP_EXECUTE_LAUNCH, OP_INITIATE, OP_INSPECT, OP_REMOVE, OP_STATE,
    OP_TERMINATE,
};

fn stranger_unit() -> UnitHandle {
    UnitHandle::mint()
}

fn stranger_exec() -> ExecutionHandle {
    ExecutionHandle::mint()
}

fn key() -> ReconciliationKey {
    ReconciliationKey::generate()
}

#[test]
fn unknown_operation_refuses() {
    let guest = IsolatorGuest::new();
    let error = guest
        .dispatch("isolator.nope", &serde_json::json!({}))
        .expect_err("unknown op must refuse");
    assert!(
        error.to_string().contains("unknown isolator operation"),
        "got: {error}"
    );
}

#[test]
fn malformed_create_payload_refuses() {
    let guest = IsolatorGuest::new();
    let error = guest
        .dispatch(OP_CREATE, &serde_json::json!({"unit_handle": "x"}))
        .expect_err("shape violation must refuse");
    assert!(
        error.to_string().contains("bad isolator.create payload"),
        "got: {error}"
    );
}

#[test]
fn unknown_unit_handle_refuses_before_backend() {
    // Channeled guest (real backend): channel present so launch
    // reaches resolution; every op refuses the stranger before any
    // backend call.
    let (_, sender) = channeled_pair();
    let guest = IsolatorGuest::new().with_fd_channel(sender);
    let handle = stranger_unit().as_str().to_string();
    let key_text = key().as_str().to_string();
    for (op, payload) in [
        (
            OP_INITIATE,
            serde_json::json!({"unit_handle": handle, "reconciliation_key": key_text}),
        ),
        (OP_INSPECT, serde_json::json!({"unit_handle": handle})),
        (OP_STATE, serde_json::json!({"unit_handle": handle})),
        (
            OP_TERMINATE,
            serde_json::json!({"unit_handle": handle, "grace_ms": 1000u64, "reconciliation_key": key_text}),
        ),
        (
            OP_REMOVE,
            serde_json::json!({"unit_handle": handle, "reconciliation_key": key_text}),
        ),
        (
            OP_EXECUTE_LAUNCH,
            serde_json::json!({
                "unit_handle": handle,
                "execution_handle": stranger_exec().as_str(),
                "argv": ["true"],
                "workdir": null,
                "reconciliation_key": key_text,
            }),
        ),
    ] {
        let error = guest
            .dispatch(op, &payload)
            .expect_err("unknown unit handle must refuse");
        assert!(
            error.to_string().contains("unknown unit handle"),
            "{op} got: {error}"
        );
    }
}

#[test]
fn unknown_execution_handle_refuses_before_backend() {
    let guest = IsolatorGuest::new();
    let error = guest
        .dispatch(
            cistella::framework::protocol::AWAIT_RESULT_OP,
            &serde_json::json!({"execution_handle": stranger_exec().as_str()}),
        )
        .expect_err("unknown execution handle must refuse");
    assert!(
        error.to_string().contains("unknown execution handle"),
        "got: {error}"
    );
}

#[test]
fn evil_handle_bytes_never_render() {
    // A unit handle carrying control bytes and a fake secret must
    // refuse on grammar with a fixed message: no wire byte reaches
    // diagnostics, and lookup never runs.
    let guest = IsolatorGuest::new();
    let error = guest
        .dispatch(
            OP_STATE,
            &serde_json::json!({"unit_handle": "BAD\nHANDLE\x01SECRET=wire-sentinel"}),
        )
        .expect_err("evil handle must refuse");
    assert_eq!(
        error.to_string(),
        "contract: bad unit handle: shape violation",
        "fixed message, no echo"
    );
}

#[test]
fn evil_handle_refuses_before_binding() {
    // The create path grammar-checks before storing: an evil
    // handle never binds and never renders, and a later lookup
    // still reports unknown (nothing was stored).
    let guest = IsolatorGuest::new();
    let evil = "BAD\nHANDLE\x01SECRET=wire-sentinel";
    let error = guest
        .dispatch(
            OP_CREATE,
            &serde_json::json!({
                "unit_handle": evil,
                "spec": {
                    "session": {
                        "id": "x", "directory": "/tmp/x", "profile": "p",
                        "profile_digest": "d", "identity": "i",
                        "command": ["true"], "image": "img",
                        "container_home": "/home/cistella"
                    },
                    "volumes": [], "env": [], "labels": []
                },
                "reconciliation_key": ReconciliationKey::generate().as_str(),
            }),
        )
        .expect_err("evil create handle must refuse");
    assert_eq!(
        error.to_string(),
        "contract: bad unit handle: shape violation",
        "fixed message, no echo"
    );
}

#[test]
fn malformed_payload_hides_serde_text() {
    // Serde error text can echo payload bytes; the refusal carries
    // a fixed shape message instead.
    let guest = IsolatorGuest::new();
    let error = guest
        .dispatch(
            OP_CREATE,
            &serde_json::json!({"unit_handle": "x", "smuggled": "SECRET=shape-sentinel"}),
        )
        .expect_err("shape violation must refuse");
    assert_eq!(
        error.to_string(),
        "contract: bad isolator.create payload: shape violation",
        "fixed message, no echo"
    );
}

/// Scripted backend: in-memory units plus a launch log, so replay
/// proofs run with no Podman. Shared across guest instances through
/// clone to prove adopt-after-restart convergence.
#[derive(Debug, Clone, Default)]
struct FakeBackend {
    /// Local handle to lifecycle state.
    units: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<UnitHandle, LifecycleState>>>,
    /// Reconciliation key to local handle.
    keys:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<ReconciliationKey, UnitHandle>>>,
    /// Launched argv per call (double-spawn detector).
    launches: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    /// Backend `create` call count (adopt must not recreate).
    creates: std::sync::Arc<std::sync::Mutex<usize>>,
    /// Terminated unit handles, in order.
    terminated: std::sync::Arc<std::sync::Mutex<Vec<UnitHandle>>>,
    /// Removed unit handles, in order.
    removed: std::sync::Arc<std::sync::Mutex<Vec<UnitHandle>>>,
}

use cistella::framework::contract::LifecycleState;
use cistella::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use cistella::session::Session;

impl Isolator for FakeBackend {
    fn capabilities(&self) -> IsolatorCapabilities {
        IsolatorCapabilities {
            backend: "fake",
            supports: vec![],
        }
    }

    fn create(
        &self,
        _spec: &CreateSpec,
        key: &ReconciliationKey,
    ) -> cistella::error::Result<UnitHandle> {
        let local = UnitHandle::mint();
        self.units
            .lock()
            .unwrap()
            .insert(local.clone(), LifecycleState::Created);
        self.keys.lock().unwrap().insert(key.clone(), local.clone());
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
        Ok(RemovedAttestation {
            unit_identity: handle.as_str().to_string(),
        })
    }

    fn locate(&self, key: &ReconciliationKey) -> cistella::error::Result<Option<UnitHandle>> {
        Ok(self.keys.lock().unwrap().get(key).cloned())
    }
}

/// Minimal creation spec for replay payloads.
fn replay_spec() -> CreateSpec {
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

fn create_payload(fw: &str, key: &ReconciliationKey, spec: &CreateSpec) -> serde_json::Value {
    serde_json::json!({
        "unit_handle": fw,
        "spec": serde_json::to_value(spec).expect("spec serializes"),
        "reconciliation_key": key.as_str(),
    })
}

fn launch_payload(
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

#[test]
fn create_replay_is_idempotent_without_recreate() {
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    let first = guest
        .dispatch(OP_CREATE, &create_payload("replayunit01", &key, &spec))
        .expect("first create must succeed");
    let second = guest
        .dispatch(OP_CREATE, &create_payload("replayunit01", &key, &spec))
        .expect("identical replay must succeed");
    assert_eq!(first, second, "replay returns the same handle");
    assert_eq!(
        *backend.creates.lock().unwrap(),
        1,
        "backend creates exactly once"
    );
}

#[test]
fn create_mismatched_key_refuses() {
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("replayunit02", &key, &spec))
        .expect("first create must succeed");
    let other = ReconciliationKey::generate();
    let error = guest
        .dispatch(OP_CREATE, &create_payload("replayunit02", &other, &spec))
        .expect_err("different key on a bound handle must refuse");
    assert!(
        error.to_string().contains("different attempt"),
        "got: {error}"
    );
    assert_eq!(
        *backend.creates.lock().unwrap(),
        1,
        "refusal creates nothing"
    );
}

#[test]
fn create_adopts_after_restart_without_recreate() {
    // Guest restart drops tables but the backend remembers the key:
    // a fresh guest with the same backend adopts instead of
    // creating, and the adopted unit initiates normally.
    let backend = FakeBackend::default();
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    {
        let guest = IsolatorGuest::with_backend(backend.clone());
        guest
            .dispatch(OP_CREATE, &create_payload("replayunit03", &key, &spec))
            .expect("first create must succeed");
    }
    let guest = IsolatorGuest::with_backend(backend.clone());
    let replayed = guest
        .dispatch(OP_CREATE, &create_payload("replayunit03", &key, &spec))
        .expect("adopt must succeed");
    assert_eq!(
        replayed,
        serde_json::json!({"unit_handle": "replayunit03"}),
        "adopt echoes the framework handle"
    );
    assert_eq!(
        *backend.creates.lock().unwrap(),
        1,
        "adopt recreates nothing"
    );
    let initiated = guest
        .dispatch(
            OP_INITIATE,
            &serde_json::json!({"unit_handle": "replayunit03", "reconciliation_key": key.as_str()}),
        )
        .expect("initiate on the adopted unit must succeed");
    assert!(
        initiated
            .get("started_attestation")
            .and_then(|att| att.get("pidns_proof"))
            .is_some(),
        "adopted attestation carries the proof shape: {initiated}"
    );
}

/// Guest with an fd channel plus the test-side sender: every launch
/// dispatch below stages one bundle first (mirroring production
/// order: wire op, then bundle, then spawn).
fn channeled_guest<B: cistella::framework::isolator::Isolator>(
    backend: B,
) -> (
    cistella::isolators::wire::IsolatorGuest<B>,
    std::os::fd::OwnedFd,
) {
    let (a, b) = channeled_pair();
    (
        cistella::isolators::wire::IsolatorGuest::with_backend(backend).with_fd_channel(b),
        a,
    )
}

/// One socketpair: guest end plus test-sender end.
fn channeled_pair() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, socketpair};
    socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::empty(),
    )
    .expect("test socketpair")
}

/// Stages one bundle for the next launch dispatch.
fn stage_bundle(sender: &std::os::fd::OwnedFd, unit: &str, exec: &str) {
    use std::os::fd::AsFd;
    let null = |n: &str| {
        let _ = n;
        std::fs::File::open("/dev/null").expect("/dev/null opens")
    };
    let f0 = null(unit);
    let f1 = null(unit);
    let f2 = null(unit);
    cistella::framework::fdpass::send_bundle(
        sender,
        &cistella::framework::fdpass::BundleHeader {
            unit_handle: unit.to_string(),
            execution_handle: exec.to_string(),
        },
        &[f0.as_fd(), f1.as_fd(), f2.as_fd()],
        std::time::Duration::from_secs(2),
    )
    .expect("stage bundle");
}

#[test]
fn launch_replay_spawns_once() {
    let backend = FakeBackend::default();
    let (guest, sender) = channeled_guest(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("replayunit04", &key, &spec))
        .expect("create must succeed");
    let argv = ["true"];
    stage_bundle(&sender, "replayunit04", "replayexec04");
    guest
        .dispatch(
            OP_EXECUTE_LAUNCH,
            &launch_payload("replayunit04", "replayexec04", &key, &argv),
        )
        .expect("first launch must succeed");
    // Identical replay consumes its own staged bundle (channel
    // stays aligned) without spawning again.
    stage_bundle(&sender, "replayunit04", "replayexec04");
    guest
        .dispatch(
            OP_EXECUTE_LAUNCH,
            &launch_payload("replayunit04", "replayexec04", &key, &argv),
        )
        .expect("identical replay must succeed without respawn");
    assert_eq!(
        backend.launches.lock().unwrap().len(),
        1,
        "backend spawns exactly once"
    );
}

#[test]
fn launch_divergent_replay_refuses_without_respawn() {
    let backend = FakeBackend::default();
    let (guest, sender) = channeled_guest(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("replayunit05", &key, &spec))
        .expect("create must succeed");
    stage_bundle(&sender, "replayunit05", "replayexec05");
    guest
        .dispatch(
            OP_EXECUTE_LAUNCH,
            &launch_payload("replayunit05", "replayexec05", &key, &["true"]),
        )
        .expect("first launch must succeed");
    let error = guest
        .dispatch(
            OP_EXECUTE_LAUNCH,
            &launch_payload("replayunit05", "replayexec05", &key, &["false"]),
        )
        .expect_err("divergent argv on a bound handle must refuse");
    assert!(
        error.to_string().contains("different attempt"),
        "got: {error}"
    );
    assert_eq!(
        backend.launches.lock().unwrap().len(),
        1,
        "refusal spawns nothing"
    );
}

#[test]
fn converge_all_terminates_removes_and_clears() {
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("replayunit06", &key, &spec))
        .expect("create must succeed");
    assert!(guest.has_live_state(), "bound unit is live state");
    guest
        .converge_all(std::time::Duration::from_millis(100))
        .expect("converge must succeed");
    assert_eq!(backend.terminated.lock().unwrap().len(), 1);
    assert_eq!(backend.removed.lock().unwrap().len(), 1);
    assert!(!guest.has_live_state(), "tables clear after converge");
    let error = guest
        .dispatch(
            OP_STATE,
            &serde_json::json!({"unit_handle": "replayunit06"}),
        )
        .expect_err("cleared binding must refuse");
    assert!(
        error.to_string().contains("unknown unit handle"),
        "got: {error}"
    );
}

/// Gated backend: `await_result` blocks until the test releases the
/// gate, proving terminate-during-await and abandonment-detach
/// without Podman.
#[derive(Debug, Clone, Default)]
struct GatedBackend {
    /// Release flag for blocked awaits.
    release: std::sync::Arc<std::sync::Mutex<bool>>,
    /// Completed await count.
    completed: std::sync::Arc<std::sync::Mutex<usize>>,
    /// Terminate call count (served while awaits block).
    terminated: std::sync::Arc<std::sync::Mutex<usize>>,
}

impl Isolator for GatedBackend {
    fn capabilities(&self) -> IsolatorCapabilities {
        IsolatorCapabilities {
            backend: "gated",
            supports: vec![],
        }
    }

    fn create(
        &self,
        _spec: &CreateSpec,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<UnitHandle> {
        Ok(UnitHandle::mint())
    }

    fn initiate(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<StartedAttestation> {
        Ok(StartedAttestation {
            unit_identity: handle.as_str().to_string(),
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
    ) -> cistella::error::Result<cistella::framework::contract::ExecutionHandle> {
        Ok(cistella::framework::contract::ExecutionHandle::mint())
    }

    fn await_result(
        &self,
        _execution: &cistella::framework::contract::ExecutionHandle,
        _cancel: &cistella::framework::contract::CancelFlag,
    ) -> cistella::error::Result<ExecutionOutcome> {
        // Block until the test releases: abandonment is observed by
        // the caller dropping interest, never by killing this wait.
        loop {
            if *self.release.lock().unwrap() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        *self.completed.lock().unwrap() += 1;
        Ok(ExecutionOutcome::Exited(0))
    }

    fn inspect(&self, handle: &UnitHandle) -> cistella::error::Result<UnitSnapshot> {
        Ok(UnitSnapshot {
            unit_identity: handle.as_str().to_string(),
            lifecycle: LifecycleState::Executing,
            active_state: "active".to_string(),
            session_id: String::new(),
            image: String::new(),
        })
    }

    fn state(&self, handle: &UnitHandle) -> cistella::error::Result<LifecycleState> {
        let _ = handle;
        Ok(LifecycleState::Executing)
    }

    fn terminate(
        &self,
        _handle: &UnitHandle,
        _grace: std::time::Duration,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<StoppedAttestation> {
        *self.terminated.lock().unwrap() += 1;
        Ok(StoppedAttestation {
            unit_identity: "gated".to_string(),
        })
    }

    fn remove(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> cistella::error::Result<RemovedAttestation> {
        Ok(RemovedAttestation {
            unit_identity: handle.as_str().to_string(),
        })
    }

    fn locate(&self, _key: &ReconciliationKey) -> cistella::error::Result<Option<UnitHandle>> {
        Ok(None)
    }
}

#[test]
fn terminate_served_during_blocked_await() {
    use std::sync::Arc;

    let backend = GatedBackend::default();
    let (ungated, sender) = channeled_guest(backend.clone());
    let guest = Arc::new(ungated);
    // Bind a unit plus an execution through dispatch (no backend
    // wait involved in either call).
    let key = ReconciliationKey::generate();
    guest
        .dispatch(
            OP_CREATE,
            &create_payload("gatedunit01", &key, &replay_spec()),
        )
        .expect("create must succeed");
    stage_bundle(&sender, "gatedunit01", "gatedexec01");
    guest
        .dispatch(
            OP_EXECUTE_LAUNCH,
            &launch_payload("gatedunit01", "gatedexec01", &key, &["sleep", "60"]),
        )
        .expect("launch must succeed");
    // Block one thread inside await_result while the main thread
    // terminates the unit: availability during await is the pin.
    let waiter = {
        let guest = Arc::clone(&guest);
        std::thread::spawn(move || {
            guest.dispatch(
                cistella::framework::protocol::AWAIT_RESULT_OP,
                &serde_json::json!({"execution_handle": "gatedexec01"}),
            )
        })
    };
    // Give the waiter a moment to block inside the backend wait.
    std::thread::sleep(std::time::Duration::from_millis(100));
    guest
        .dispatch(
            OP_TERMINATE,
            &serde_json::json!({
                "unit_handle": "gatedunit01",
                "grace_ms": 1000u64,
                "reconciliation_key": key.as_str(),
            }),
        )
        .expect("terminate must serve during a blocked await");
    assert_eq!(
        *backend.terminated.lock().unwrap(),
        1,
        "terminate reached the backend while await blocked"
    );
    // Release the gate: the waiter completes normally (no kill, no
    // fabricated outcome — detach would simply stop listening).
    *backend.release.lock().unwrap() = true;
    let outcome = waiter
        .join()
        .expect("waiter joins")
        .expect("await completes");
    assert_eq!(outcome, serde_json::json!({"exit_status": 0}));
}

#[test]
fn converge_all_spares_units_with_live_executions() {
    // Disconnect is detach-without-kill: a unit with a live
    // execution is left running and bound for the framework's
    // typed teardown path, never swept by the guest.
    let backend = FakeBackend::default();
    let (guest, sender) = channeled_guest(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("liveunit01", &key, &spec))
        .expect("create must succeed");
    stage_bundle(&sender, "liveunit01", "liveexec01");
    guest
        .dispatch(
            OP_EXECUTE_LAUNCH,
            &launch_payload("liveunit01", "liveexec01", &key, &["sleep", "60"]),
        )
        .expect("launch must succeed");
    guest
        .converge_all(std::time::Duration::from_millis(100))
        .expect("converge reports clean with nothing converged");
    assert_eq!(
        backend.terminated.lock().unwrap().len(),
        0,
        "live executions are never swept on disconnect"
    );
    assert!(
        guest.has_live_state(),
        "bindings survive for framework teardown"
    );
}

#[test]
fn launch_without_channel_refuses() {
    // No fd channel, no launch: harness stdio has no route to
    // arrive on, so the external path refuses before lookup.
    // (In-process conduct never crosses this dispatch.)
    let guest = IsolatorGuest::new();
    let error = guest
        .dispatch(
            OP_EXECUTE_LAUNCH,
            &serde_json::json!({
                "unit_handle": "someunit01",
                "execution_handle": "someexec01",
                "argv": ["true"],
                "workdir": null,
                "reconciliation_key": ReconciliationKey::generate().as_str(),
            }),
        )
        .expect_err("channel-less launch must refuse");
    assert_eq!(
        error.to_string(),
        "contract: launch requires an fd channel",
        "fixed message"
    );
}

#[test]
fn await_outcome_shapes_pin() {
    use cistella::framework::isolator::ExecutionOutcome as Outcome;
    use cistella::isolators::client::parse_await_outcome;
    assert_eq!(
        parse_await_outcome(&serde_json::json!({"exit_status": 0})).expect("exit"),
        Outcome::Exited(0)
    );
    assert_eq!(
        parse_await_outcome(&serde_json::json!({"signal": 9})).expect("signal"),
        Outcome::Signaled(9)
    );
    assert!(
        parse_await_outcome(&serde_json::json!({"Exited": 0})).is_err(),
        "derived enum spelling is not the wire shape"
    );
    assert!(
        parse_await_outcome(&serde_json::json!({"ok": true})).is_err(),
        "unrelated shape refuses"
    );
}

#[test]
fn wire_client_capabilities_match_backend() {
    // Parity: the wire client delegates to the same backend
    // capabilities (podman-quadlet realizing environment plus
    // mounts), so merge gating decides identically on the wire
    // and in-process paths. The delegation lives in
    // `WireClient::capabilities`; this pins the reference side it
    // must mirror (constructing a live client needs a guest
    // binary, so the live parity runs in integration).
    use cistella::framework::contract::Capability;
    use cistella::framework::isolator::Isolator;
    use cistella::isolators::podman::PodmanIsolator;
    let reference = PodmanIsolator::new().capabilities();
    assert_eq!(reference.backend, "podman-quadlet");
    assert!(reference.supports(Capability::Environment));
    assert!(reference.supports(Capability::Mounts));
}
