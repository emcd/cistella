//! Wire death-gate proofs: latch taxonomy plus conduct residue dominance.
//!
//! Split from `guest_hosting.rs` at the file-size limit. Fast tests
//! drive the scripted peer (simultaneous death, stall/timeout,
//! local oversize refusal, close-after-death); the ignored live
//! test drives the real guest plus backend for crash-after-create
//! residue dominance with name-based converge.

use std::time::Duration;

use cistella::framework::contract::Deadlines;

use super::protocol_peer::peer_path;

fn tight_deadlines() -> Deadlines {
    Deadlines {
        hello: Duration::from_secs(2),
        plan: Duration::from_secs(2),
        apply: Duration::from_secs(2),
        terminate_grace: Duration::from_secs(2),
        frame_completion: Duration::from_secs(60),
    }
}

/// The examples directory holding the peer binary.
fn examples_dir() -> std::path::PathBuf {
    peer_path()
        .parent()
        .expect("peer has a parent directory")
        .to_path_buf()
}

/// Directory holding built binaries (`target/<profile>/`): the
/// peer lives in `examples/`, the guest binary beside it.
fn bins_dir() -> std::path::PathBuf {
    peer_path()
        .parent()
        .expect("examples dir")
        .parent()
        .expect("profile dir")
        .to_path_buf()
}

/// Resolves the built isolator guest binary (built by the normal
/// test-target build; a missing binary is an environment bug, not
/// a skip — fail loudly with the build instruction).
fn guest_bin() -> (std::path::PathBuf, String) {
    let dir = bins_dir();
    let name = cistella::isolators::client::ISOLATOR_BIN.to_string();
    assert!(
        dir.join(&name).exists(),
        "guest binary missing: run `cargo build --bin {name}` first"
    );
    (dir, name)
}

#[test]
fn wire_client_two_inflight_calls_fail_typed_on_death() {
    // The scripted peer dies after its SECOND op with no replies:
    // both callers are genuinely in flight at the moment of death
    // (the peer itself waits for both frames — no sleep-tuned
    // sequencing on this side). Both must fail typed (never hang),
    // and the death latch must trip so conduct's residue gate can
    // rely on it without matching error text.
    use cistella::framework::contract::ReconciliationKey;
    use cistella::framework::isolator::{CreateSpec, Isolator};
    use cistella::isolators::client::WireClient;
    use cistella::session::Session;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let mut deadlines = tight_deadlines();
    deadlines.apply = Duration::from_secs(15);
    let client = std::sync::Arc::new(
        WireClient::host_as(
            &dir,
            &name,
            &[
                "--mode=isolator-die-after-two".to_string(),
                format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
            ],
            rendezvous.path(),
            deadlines,
        )
        .expect("host must negotiate with the scripted peer"),
    );
    let spec = CreateSpec {
        session: Session {
            id: "wireclient02".to_string(),
            directory: "/tmp/wireclient02".to_string(),
            profile: "probe".to_string(),
            profile_digest: "digest".to_string(),
            identity: "tester".to_string(),
            command: vec!["true".to_string()],
            image: "localhost/cistella/opencode:example".to_string(),
            container_home: "/home/cistella".to_string(),
        },
        volumes: vec![],
        env: vec![],
        labels: vec![],
    };
    let creator = {
        let client = std::sync::Arc::clone(&client);
        std::thread::spawn(move || client.create(&spec, &ReconciliationKey::generate()))
    };
    let inspector = {
        let client = std::sync::Arc::clone(&client);
        std::thread::spawn(move || {
            client.inspect(&cistella::framework::contract::UnitHandle::mint())
        })
    };
    let created = creator
        .join()
        .expect("creator joins")
        .expect_err("create in flight at death must fail typed");
    let inspected = inspector
        .join()
        .expect("inspector joins")
        .expect_err("inspect in flight at death must fail typed");
    for (which, error) in [("create", created), ("inspect", inspected)] {
        assert!(
            matches!(error, cistella::error::CistellaError::Protocol(_)),
            "{which} must fail as typed Protocol, got: {error}"
        );
        assert!(
            error.to_string().contains("guest terminated")
                || error.to_string().contains("dispatcher")
                || error.to_string().contains("guest send failed"),
            "{which} names the death, got: {error}"
        );
    }
    assert!(
        client.guest_dead(),
        "dispatcher must trip the death latch on abnormal exit"
    );
    drop(client);
}

/// Best-effort converge for a wire-created live unit (no registry
/// record exists — conduct never ran — and no in-process handle
/// record either, so only name-based `runtime::teardown` owns the
/// cleanup, never a fresh isolator with an unknown handle).
/// Armed until explicitly disarmed; teardown is idempotent.
struct LiveUnitGuard {
    container_name: Option<String>,
    session_id: Option<String>,
}

impl Drop for LiveUnitGuard {
    fn drop(&mut self) {
        if let (Some(container), Some(id)) = (self.container_name.take(), self.session_id.take()) {
            let _ = cistella::runtime::teardown(&container, &id);
        }
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn wire_client_crash_after_create_residue_dominates() {
    // Real guest, real backend: create a unit through the wire,
    // SIGKILL the guest, then prove the conduct death gate —
    // every later op fails typed, the latch trips, and the
    // residue check (not the raw death error) dominates the
    // report. Cleanup converges directly (no living guest).
    use cistella::framework::contract::ReconciliationKey;
    use cistella::framework::isolator::{CreateSpec, Isolator};
    use cistella::isolators::client::WireClient;
    use cistella::session::{Session, mint_session_id};
    if !super::helpers::systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let (dir, _name) = guest_bin();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let client = WireClient::host(&dir, rendezvous.path(), Default::default())
        .expect("host must negotiate with the real guest");
    let session_id = mint_session_id();
    let session = Session {
        id: session_id.clone(),
        directory: "/tmp/wireclient-live".to_string(),
        profile: "probe".to_string(),
        profile_digest: "digest".to_string(),
        identity: "tester".to_string(),
        command: vec!["true".to_string()],
        image: "localhost/cistella/opencode:example".to_string(),
        container_home: "/home/cistella".to_string(),
    };
    let container_name = session.container_name();
    let spec = CreateSpec {
        session,
        volumes: vec![],
        env: vec![],
        labels: vec![],
    };
    let key = ReconciliationKey::generate();
    // Arm the name-based guard BEFORE create: a partial install
    // that returns Err still leaves state, and only teardown by
    // name converges it (no handle record exists anywhere). The
    // guard disarms only after the residue assertion verifies gone.
    let mut guard = LiveUnitGuard {
        container_name: Some(container_name.clone()),
        session_id: Some(session_id.clone()),
    };
    let unit = client
        .create(&spec, &key)
        .expect("live create must install the unit");
    // Kill the guest by its rendezvous argument (unique per test):
    // the crash lands with no ops in flight — simultaneity at
    // death is pinned by the scripted fast test; here the real
    // backend proves residue dominance.
    let killed = std::process::Command::new("pkill")
        .arg("-f")
        .arg(rendezvous.path().to_string_lossy().as_ref())
        .status()
        .expect("pkill runs")
        .success();
    assert!(killed, "pkill must match the guest");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !client.guest_dead() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(client.guest_dead(), "dispatcher must observe the kill");
    let stated = client.state(&unit).map(|_| ());
    let inspected = client.inspect(&unit).map(|_| ());
    for (which, outcome) in [("state", stated), ("inspect", inspected)] {
        let error = outcome.expect_err(&format!("{which} after the kill must fail typed"));
        assert!(
            matches!(error, cistella::error::CistellaError::Protocol(_)),
            "{which} must fail as typed Protocol, got: {error}"
        );
    }
    // The conduct gate on a post-death outcome: residue (the unit
    // create installed) dominates the raw death error.
    let dominated = client.death_checked(client.inspect(&unit).map(|_| ()));
    let error = dominated.expect_err("residue must dominate after crash-after-create");
    assert!(
        matches!(error, cistella::error::CistellaError::Contract(_)),
        "residue dominates as Contract, got: {error}"
    );
    assert!(
        error.to_string().contains("residue"),
        "residue named, got: {error}"
    );
    // Direct converge by name (no living guest, no handle record
    // anywhere): the full backend teardown, then gone. The guard
    // disarms only after the residue assertion verifies clean — a
    // teardown-Ok with remaining residue still retries on panic.
    cistella::runtime::teardown(&container_name, &session_id).expect("direct teardown converges");
    assert!(
        cistella::runtime::residue_gone(&container_name, &session_id),
        "no residue after direct converge"
    );
    guard.container_name = None;
    guard.session_id = None;
    let _ = client.close();
}

#[test]
fn wire_client_send_timeout_is_fatal_and_latches() {
    // The scripted peer stalls without reading while the test
    // sends a pipe-filling create (over the 64KiB pipe buffer, far
    // under the 8MiB ceiling): the write blocks past the apply
    // deadline with partial bytes already emitted. Neither end
    // resynchronizes a timed-out stream, so the dispatcher treats
    // it as fatal — bounded shutdown/reap first, then latch and
    // fail typed. The latch means dead-or-reaped (never "maybe
    // alive"); close then reports the dead exchange while still
    // joining and unlinking.
    // Timing note: the guest stdin is nonblocking, so the
    // oversized write retries against the apply deadline instead
    // of sleeping in the kernel past it — expect ~2s on any
    // kernel. (This seat once parked a *blocking* pipe-writer
    // until reader exit, which is exactly the hole nonblocking
    // writers close; see GuestHost::spawn.)
    //
    // The pin is the fatal-exchange shape (typed fail, latch,
    // close reports, path unlinked), never the wall time.
    use cistella::framework::contract::ReconciliationKey;
    use cistella::framework::isolator::{CreateSpec, Isolator};
    use cistella::isolators::client::WireClient;
    use cistella::session::Session;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let mut deadlines = tight_deadlines();
    deadlines.apply = Duration::from_secs(2);
    // Elapsed bound: the framework's 2s apply deadline must bound
    // this call, not the peer's 30s lifetime (10s bound = 5x
    // margin; pre-nonblocking-writer this took the full 30s).
    let started = std::time::Instant::now();
    let client = WireClient::host_as(
        &dir,
        &name,
        &[
            "--mode=isolator-create-stall".to_string(),
            format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
        ],
        rendezvous.path(),
        deadlines,
    )
    .expect("host must negotiate with the scripted peer");
    // Pipe-filling payload: 220KiB of env padding — blocks the
    // writer once the pipe buffer fills, instead of fitting
    // instantly (which would expire as an op timeout, a different
    // and non-fatal path).
    let pad: Vec<String> = (0..2000)
        .map(|i| format!("CISTELLA_PAD{i:04}={}", "x".repeat(100)))
        .collect();
    let spec = CreateSpec {
        session: Session {
            id: "wireclient03".to_string(),
            directory: "/tmp/wireclient03".to_string(),
            profile: "probe".to_string(),
            profile_digest: "digest".to_string(),
            identity: "tester".to_string(),
            command: vec!["true".to_string()],
            image: "localhost/cistella/opencode:example".to_string(),
            container_home: "/home/cistella".to_string(),
        },
        volumes: vec![],
        env: pad,
        labels: vec![],
    };
    let error = client
        .create(&spec, &ReconciliationKey::generate())
        .expect_err("stalled send must fail fatal");
    assert!(
        matches!(error, cistella::error::CistellaError::Protocol(_)),
        "fatal timeout must fail as typed Protocol, got: {error}"
    );
    assert!(
        error.to_string().contains("timed out"),
        "timeout named, got: {error}"
    );
    assert!(
        client.guest_dead(),
        "fatal timeout trips the latch after quiesce"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "framework deadline bounds the call, not peer lifetime"
    );
    let error = client
        .close()
        .expect_err("close after fatal timeout reports the dead exchange");
    assert!(
        matches!(error, cistella::error::CistellaError::Protocol(_)),
        "close-after-death must fail as typed Protocol, got: {error}"
    );
    assert!(
        std::fs::read_dir(rendezvous.path())
            .expect("rendezvous dir lists")
            .next()
            .is_none(),
        "rendezvous socket unlinked on every close outcome"
    );
}

#[test]
fn wire_client_oversize_refused_locally_latch_clear() {
    // The peer promises a 256-byte ceiling and serves state: the
    // create payload (hundreds of bytes of session spec) exceeds
    // it, so the client must refuse LOCALLY with typed Contract —
    // never sent, guest untouched, latch clear — while a small
    // state op still succeeds through the same dispatcher after.
    use cistella::framework::contract::{LifecycleState, ReconciliationKey};
    use cistella::framework::isolator::{CreateSpec, Isolator};
    use cistella::isolators::client::WireClient;
    use cistella::session::Session;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let client = WireClient::host_as(
        &dir,
        &name,
        &[
            "--mode=isolator-small-ceiling".to_string(),
            format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
        ],
        rendezvous.path(),
        tight_deadlines(),
    )
    .expect("host must negotiate with the scripted peer");
    let spec = CreateSpec {
        session: Session {
            id: "wireclient04".to_string(),
            directory: "/tmp/wireclient04".to_string(),
            profile: "probe".to_string(),
            profile_digest: "digest".to_string(),
            identity: "tester".to_string(),
            command: vec!["true".to_string()],
            image: "localhost/cistella/opencode:example".to_string(),
            container_home: "/home/cistella".to_string(),
        },
        volumes: vec![],
        env: vec![],
        labels: vec![],
    };
    let error = client
        .create(&spec, &ReconciliationKey::generate())
        .expect_err("oversize create must refuse locally");
    assert!(
        matches!(error, cistella::error::CistellaError::Contract(_)),
        "local refusal must fail as typed Contract, got: {error}"
    );
    assert!(
        error.to_string().contains("exceeds maximum"),
        "ceiling named, got: {error}"
    );
    assert!(
        !client.guest_dead(),
        "a local refusal must not trip the death latch"
    );
    let lifecycle = client
        .state(&cistella::framework::contract::UnitHandle::mint())
        .expect("dispatcher still serves after local refusal");
    assert_eq!(lifecycle, LifecycleState::Initiated);
    client.close().expect("orderly close");
}

#[test]
fn wire_client_close_after_death_reports() {
    // The peer exits right after hello: once the dispatcher
    // observes the death (latch poll, no sleep-tuned sequencing),
    // close must REPORT (Err) rather than Ok — the success-path
    // release gate depends on this outcome to dominate a clean
    // harness when the guest's shutdown detected residue.
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let client = WireClient::host_as(
        &dir,
        &name,
        &[
            "--mode=isolator-exit-after-hello".to_string(),
            format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
        ],
        rendezvous.path(),
        tight_deadlines(),
    )
    .expect("host must negotiate before the peer exits");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !client.guest_dead() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(client.guest_dead(), "dispatcher must observe the exit");
    let error = client
        .close()
        .expect_err("close after death must report, not Ok");
    assert!(
        matches!(error, cistella::error::CistellaError::Protocol(_)),
        "close-after-death must fail as typed Protocol, got: {error}"
    );
}

#[test]
fn teardown_unit_lock_held_branch_completes_under_guard() {
    // Production-branch pin for the prepare-arm converge: with a
    // dead guest and the creation-window guard HELD,
    // `teardown_unit(lock_held=true)` must take the lock-held half
    // (`teardown_inner`). A regression swapping it to the full
    // converge re-acquires the guard and wedges — the join timeout
    // fails loudly instead of hanging the suite. Completion (not
    // the value) is the assertion; the unit mechanism pin lives
    // beside it in `tests/unit/lock_teardown.rs`.
    use cistella::framework::contract::ReconciliationKey;
    use cistella::framework::isolator::Isolator;
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let mut deadlines = tight_deadlines();
    deadlines.apply = Duration::from_secs(15);
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let client = WireClient::host_as(
        &dir,
        &name,
        &[
            "--mode=isolator-die-after-two".to_string(),
            format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
        ],
        rendezvous.path(),
        deadlines,
    )
    .expect("host must negotiate with the scripted peer");
    // Establish death with two genuinely in-flight ops (the peer
    // waits for both frames before dying — no sleep-tuned
    // sequencing).
    let creator = {
        let client = &client;
        std::thread::scope(|scope| {
            let first =
                scope.spawn(|| client.inspect(&cistella::framework::contract::UnitHandle::mint()));
            let second =
                scope.spawn(|| client.inspect(&cistella::framework::contract::UnitHandle::mint()));
            (
                first.join().expect("first joins"),
                second.join().expect("second joins"),
            )
        })
    };
    assert!(
        creator.0.is_err() && creator.1.is_err(),
        "both in-flight ops must fail on death"
    );
    assert!(client.guest_dead(), "death latch trips");
    let _guard = cistella::lock::LockGuard::acquire().expect("lock acquires");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = client.teardown_unit(
            &cistella::framework::contract::UnitHandle::mint(),
            Duration::from_secs(2),
            &ReconciliationKey::generate(),
            "no-such-container",
            "no-such-session",
            true,
        );
        let _ = done_tx.send(outcome.is_err());
    });
    done_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("lock-held teardown must complete, not wedge");
}

#[test]
fn wire_client_shutdown_failure_latches_uncertain_not_dead() {
    // The scripted peer reads one op (left pending), then forks a
    // setsid grandchild holding stdout while the parent exits: the
    // group kill cannot touch the reparented child, so shutdown's
    // pipe-EOF verification fails and the fatal path must NOT
    // latch death (the descendant may live) — it names the
    // shutdown failure instead, sets shutdown-uncertain, records
    // the proof failure, and still converges by name without ever
    // returning clean. The grandchild self-exits in 10s.
    use cistella::framework::contract::ReconciliationKey;
    use cistella::framework::isolator::Isolator;
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let mut deadlines = tight_deadlines();
    deadlines.apply = Duration::from_secs(15);
    let client = WireClient::host_as(
        &dir,
        &name,
        &[
            "--mode=isolator-lingering-descendant".to_string(),
            format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
        ],
        rendezvous.path(),
        deadlines,
    )
    .expect("host must negotiate with the scripted peer");
    // First op pends (peer reads it, never answers); the parent
    // exits microseconds after reading, so by the time the second
    // op sends the stdin read ends are gone and it fails fast
    // EPIPE into the fatal path. The 2s settle is not a race —
    // any value from milliseconds up behaves identically; only
    // pathological descheduling past the 15s op budget would
    // expire instead, failing loud rather than wrong.
    let waiter = {
        let client = &client;
        std::thread::scope(|scope| {
            let first =
                scope.spawn(|| client.inspect(&cistella::framework::contract::UnitHandle::mint()));
            // Let the first op arrive and the parent exit before
            // the second send; the new peer needs no sleep-tuned
            // wait — any send after parent death fails fast.
            std::thread::sleep(Duration::from_secs(2));
            let second =
                scope.spawn(|| client.inspect(&cistella::framework::contract::UnitHandle::mint()));
            (
                first.join().expect("first joins"),
                second.join().expect("second joins"),
            )
        })
    };
    for (which, outcome) in [("first", waiter.0), ("second", waiter.1)] {
        let error = outcome.expect_err(&format!("{which} must fail on fatal shutdown"));
        assert!(
            matches!(error, cistella::error::CistellaError::Protocol(_)),
            "{which} must fail as typed Protocol, got: {error}"
        );
        assert!(
            error.to_string().contains("shutdown failed"),
            "{which} must name the shutdown failure, got: {error}"
        );
    }
    assert!(
        !client.guest_dead(),
        "unproven shutdown must not latch death"
    );
    assert!(
        client.shutdown_uncertain(),
        "failed shutdown proof sets the uncertain flag"
    );
    // Production-method converge under the uncertain flag: best
    // effort by name, never clean — the shutdown residue dominates
    // even though no unit exists here (no podman in-seat means the
    // converge errors fast, which is exactly the unverified path).
    let _guard = cistella::lock::LockGuard::acquire().expect("lock acquires");
    let outcome = client.teardown_unit(
        &cistella::framework::contract::UnitHandle::mint(),
        Duration::from_secs(2),
        &ReconciliationKey::generate(),
        "no-such-container",
        "no-such-session",
        true,
    );
    let error = outcome.expect_err("uncertain teardown must never be clean");
    assert!(
        error.to_string().contains("shutdown failed"),
        "shutdown residue dominates, got: {error}"
    );
    drop(_guard);
    // Close surfaces the recorded proof failure instead of a bland
    // termination, and still joins + unlinks.
    let error = client
        .close()
        .expect_err("close after uncertain shutdown reports");
    assert!(
        error.to_string().contains("shutdown failed"),
        "close reports the recorded failure, got: {error}"
    );
    assert!(
        std::fs::read_dir(rendezvous.path())
            .expect("rendezvous dir lists")
            .next()
            .is_none(),
        "rendezvous socket unlinked on every close outcome"
    );
}
