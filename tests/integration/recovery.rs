//! Pre-exec recovery proofs: kill the guest, converge by key.
//!
//! These prove the primitives conduct's pre-exec episode composes
//! (kill mid-create with unknown applied state, kill after
//! initiate, second death fails stop, death at launch runs the
//! harness exactly once, and the full conduct episode re-hosting
//! through a kill in its start-delay window). The episode's own
//! verdict/single-replacement bound is pinned fast in
//! `tests/unit/terminal.rs`; here the real guest plus backend show
//! adoption converging and teardown refusing resurrection.
//!
//! Timing discipline: kills land wherever they land (that IS the
//! unknown-applied scenario) and every assertion covers all
//! resulting branches; readiness waits poll harness-visible state
//! (unit files, marker files, latch flags), never sleeps.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use cistella::framework::contract::{CancelFlag, LifecycleState, ReconciliationKey};
use cistella::framework::isolator::{CreateSpec, Isolator, StdioBinding};
use cistella::isolators::client::{ISOLATOR_BIN, WireClient};
use cistella::isolators::quadlet::residue_gone;
use cistella::mount::{MountMode, MountTriple, podman_volume_args};
use cistella::session::{Session, mint_session_id};

use super::helpers::*;
use super::protocol_peer::peer_path;

const FIXTURE_IMAGE: &str = "localhost/cistella/opencode:example";

/// Directory holding built binaries (`target/<profile>/`).
fn bins_dir() -> PathBuf {
    peer_path()
        .parent()
        .expect("examples dir")
        .parent()
        .expect("profile dir")
        .to_path_buf()
}

/// Hosts the real guest binary for one recovery test.
fn host_wire(rendezvous: &TempDir) -> WireClient {
    let dir = bins_dir();
    let name = ISOLATOR_BIN;
    assert!(
        dir.join(name).exists(),
        "guest binary missing: run `cargo build --bin {name}` first"
    );
    WireClient::host(&dir, rendezvous.path(), Default::default()).expect("host real guest")
}

/// Best-effort name-based converge (no handle record exists
/// outside the test). Armed until disarmed.
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

/// SIGKILLs the guest hosted under `rendezvous` (unique per test):
/// a true crash — no cleanup, unknown applied state.
fn kill_guest(rendezvous: &TempDir) {
    let pattern = rendezvous.path().to_string_lossy().to_string();
    let killed = Command::new("pkill")
        .args(["-9", "-f", &pattern])
        .status()
        .expect("pkill runs")
        .success();
    assert!(killed, "pkill must match the guest");
}

/// SIGKILLs the guest that is a direct child of `parent_pid`
/// (used when no rendezvous path is known test-side).
/// SIGKILLs the guest that is a direct child of `parent_pid`
/// (used when no rendezvous path is known test-side). Matches the
/// rendezvous argument, NOT the binary name: the guest is spawned
/// via `/proc/self/fd/N`, so its argv[0] never contains the binary
/// name and a bin-name pattern can never match.
fn kill_guest_child_of(parent_pid: u32) {
    let killed = Command::new("pkill")
        .args(["-9", "-P", &parent_pid.to_string(), "-f", "cistella-rdv-"])
        .status()
        .expect("pkill runs")
        .success();
    assert!(killed, "pkill must match the guest child");
}

/// Polls until the client observes the kill (latch or uncertain).
fn wait_death_observed(client: &WireClient) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.guest_dead() && !client.shutdown_uncertain() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        client.guest_dead() || client.shutdown_uncertain(),
        "dispatcher must observe the kill"
    );
}

/// Builds a session plus volumes over a worktree tempdir.
fn plan_session(image: &str, worktree: &TempDir) -> (Session, Vec<String>) {
    let id = mint_session_id();
    let directory = worktree.path().to_string_lossy().to_string();
    std::fs::write(worktree.path().join("marker"), "marker").expect("write marker");
    let session = Session {
        id: id.clone(),
        directory: directory.clone(),
        profile: "conformance".to_string(),
        profile_digest: "conformance-test".to_string(),
        identity: "conformance".to_string(),
        command: vec!["sleep".to_string(), "infinity".to_string()],
        image: image.to_string(),
        container_home: "/home/cistella".to_string(),
    };
    let scratch = cistella::lock::scratch_dir(&id)
        .to_string_lossy()
        .to_string();
    let triples = vec![
        MountTriple {
            host_source: directory,
            container_target: "/work".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: scratch,
            container_target: "/tmp/scratch".to_string(),
            mode: MountMode::Rw,
        },
    ];
    let volumes = podman_volume_args(&triples, &session.container_home.clone(), None);
    (session, volumes)
}

fn fixture_image() -> Option<String> {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return None;
    }
    match cistella::isolators::quadlet::resolve_image_digest(FIXTURE_IMAGE) {
        Ok(image) => Some(image),
        Err(error) => {
            eprintln!("skip: fixture image unavailable: {error}");
            None
        }
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn recovery_create_unknown_applied_converges() {
    // Kill mid-create: the applied state is genuinely unknown
    // (pre-install, mid-install, post-install). A replacement
    // client presenting the same key converges every branch —
    // fresh install, adopt, or fail-closed typed refusal with the
    // partial state converged directly by name.
    let Some(image) = fixture_image() else { return };
    let rendezvous = TempDir::new().expect("tempdir");
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree);
    let container = session.container_name();
    let session_id = session.id.clone();
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
    };
    // Name guard older than the client guard: unwind closes the
    // guest before converging by name (see parity `launch_wire_unit`).
    let mut guard = LiveUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    // Close-on-unwind from hosting onward: any panic before the
    // explicit close still joins the worker and unlinks rendezvous
    // through Drop. The scoped creator below borrows (no Arc).
    let mut holder = CloseGuard::new(host_wire(&rendezvous));
    // Scoped overlap (not detached): the creator thread borrows,
    // the scope joins it even on panic, and the guard still owns
    // the client on every path.
    let first_outcome = std::thread::scope(|scope| {
        let creator = scope.spawn(|| holder.get().create(&spec, &key));
        kill_guest(&rendezvous);
        creator.join().expect("creator joins")
    });
    assert!(
        first_outcome.is_err(),
        "create outstanding at the kill must fail"
    );
    let _ = holder.take().close();

    // Replacement client, same key and spec: every branch converges.
    let rendezvous2 = TempDir::new().expect("tempdir");
    let mut second = CloseGuard::new(host_wire(&rendezvous2));
    let second_ref = second.get();
    match second_ref.create(&spec, &key) {
        Ok(adopted) => {
            assert_eq!(
                second_ref
                    .inspect(&adopted)
                    .expect("inspect converged")
                    .unit_identity,
                container
            );
            let attestation = second_ref
                .initiate(&adopted, &key)
                .expect("initiate converged");
            assert!(attestation.ready);
            assert_eq!(
                second_ref.state(&adopted).expect("state after initiate"),
                LifecycleState::Initiated
            );
            second_ref
                .converge_clean(&adopted, Duration::from_secs(10), &key)
                .expect("converge");
        }
        Err(error) => {
            // Fail-closed partial install (unit file present but the
            // key binding unscannable): typed refusal, never a blind
            // duplicate — converge the partial state directly.
            assert!(
                matches!(
                    error,
                    cistella::error::CistellaError::Runtime(_)
                        | cistella::error::CistellaError::Contract(_)
                ),
                "partial install refuses typed, got: {error}"
            );
            cistella::runtime::teardown(&container, &session_id).expect("direct converge");
        }
    }
    assert!(
        residue_gone(&container, &session_id),
        "no residue after convergence"
    );
    guard.container_name = None;
    guard.session_id = None;
    let _ = second.take().close();
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn recovery_initiate_started_converges_by_adopt() {
    // Kill after initiate returned: the unit runs while both guests
    // are gone. Replacement adopts by key (new handle, same unit
    // identity), re-initiate is an idempotent no-op start, and the
    // session converges through the adopted handle.
    let Some(image) = fixture_image() else { return };
    let rendezvous = TempDir::new().expect("tempdir");
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree);
    let container = session.container_name();
    let session_id = session.id.clone();
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
    };
    let mut guard = LiveUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    let mut first = CloseGuard::new(host_wire(&rendezvous));
    let created = first.get().create(&spec, &key).expect("create");
    first.get().initiate(&created, &key).expect("initiate");
    kill_guest(&rendezvous);
    wait_death_observed(first.get());
    let _ = first.take().close();

    let rendezvous2 = TempDir::new().expect("tempdir");
    let mut second = CloseGuard::new(host_wire(&rendezvous2));
    let adopted = second
        .get()
        .create(&spec, &key)
        .expect("adopt running unit");
    assert_ne!(
        adopted.as_str(),
        created.as_str(),
        "adopt mints a new framework handle"
    );
    let attestation = second.get().initiate(&adopted, &key).expect("re-initiate");
    assert!(attestation.ready);
    assert_eq!(attestation.unit_identity, container);
    assert_eq!(
        second.get().state(&adopted).expect("state"),
        LifecycleState::Initiated
    );
    second
        .get()
        .converge_clean(&adopted, Duration::from_secs(10), &key)
        .expect("converge adopted");
    assert!(
        residue_gone(&container, &session_id),
        "no residue after convergence"
    );
    guard.container_name = None;
    guard.session_id = None;
    let _ = second.take().close();
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn recovery_second_death_fails_stop_with_residue() {
    // The bounded-replacement primitive: after adopting through one
    // death, a second death fails stop — later ops fail typed and
    // the residue check (not the raw death error) dominates, so a
    // replacement would adopt rather than pass silently.
    let Some(image) = fixture_image() else { return };
    let rendezvous = TempDir::new().expect("tempdir");
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree);
    let container = session.container_name();
    let session_id = session.id.clone();
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
    };
    // Name guard older than the client guard: unwind closes the
    // guest before converging by name.
    let mut guard = LiveUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    let mut first = CloseGuard::new(host_wire(&rendezvous));
    first.get().create(&spec, &key).expect("create");
    kill_guest(&rendezvous);
    wait_death_observed(first.get());
    let _ = first.take().close();

    let rendezvous2 = TempDir::new().expect("tempdir");
    let mut second = CloseGuard::new(host_wire(&rendezvous2));
    let adopted = second
        .get()
        .create(&spec, &key)
        .expect("adopt after first death");
    kill_guest(&rendezvous2);
    wait_death_observed(second.get());
    // SIGKILL reaps clean: the residue assertion below needs proven
    // death (uncertainty skips the keyed scan by design).
    assert!(
        second.get().guest_dead() && !second.get().shutdown_uncertain(),
        "kill must latch proven death, never uncertainty"
    );
    // Second death: ops fail typed, and the conduct gate names
    // residue (the adopted unit survives its guest).
    let error = second
        .get()
        .initiate(&adopted, &key)
        .expect_err("initiate after second death must fail");
    assert!(
        matches!(error, cistella::error::CistellaError::Protocol(_)),
        "typed protocol failure, got: {error}"
    );
    let dominated = second
        .get()
        .death_checked(second.get().inspect(&adopted).map(|_| ()));
    let error = dominated.expect_err("residue must dominate after second death");
    assert!(
        matches!(error, cistella::error::CistellaError::Contract(_))
            && error.to_string().contains("residue"),
        "residue dominates, got: {error}"
    );
    cistella::runtime::teardown(&container, &session_id).expect("direct converge");
    assert!(
        residue_gone(&container, &session_id),
        "no residue after direct converge"
    );
    guard.container_name = None;
    guard.session_id = None;
    let _ = second.take().close();
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn recovery_launch_death_runs_harness_once() {
    // Death at the launch boundary: the harness may already run
    // (spawn precedes reply), so the framework tears down typed and
    // never re-launches. The run-once marker proves exactly one
    // execution happened — no resurrection, no double run.
    let Some(image) = fixture_image() else { return };
    let rendezvous = TempDir::new().expect("tempdir");
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree);
    let container = session.container_name();
    let session_id = session.id.clone();
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
    };
    // Name guard older than the client guard: unwind closes the
    // guest before converging by name.
    let mut guard = LiveUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    let mut holder = CloseGuard::new(host_wire(&rendezvous));
    let unit = holder.get().create(&spec, &key).expect("create");
    holder.get().initiate(&unit, &key).expect("initiate");
    let launch_key = ReconciliationKey::generate();
    let launched = holder
        .get()
        .execute_launch(
            &unit,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo run >> /work/runs; sleep 30".to_string(),
            ],
            Some("/work"),
            StdioBinding::Inherit,
            &launch_key,
        )
        .expect("launch run-once harness");
    // Harness-visible readiness in the WORKTREE mount (survives
    // scratch removal): the marker proves the spawn happened
    // before the kill lands. A scratch marker would be deleted by
    // the very teardown under test.
    let marker = worktree.path().join("runs");
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::fs::read_to_string(&marker)
        .unwrap_or_default()
        .is_empty()
    {
        if Instant::now() > deadline {
            panic!("harness never marked its run");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    kill_guest(&rendezvous);
    wait_death_observed(holder.get());
    // Capture everything into locals BEFORE any failure assertion;
    // teardown and close run before the asserts below, and the
    // name-based guard disarms only on verified clean.
    let awaited = holder.get().await_result(&launched, &CancelFlag::default());
    let runs_count = std::fs::read_to_string(&marker)
        .map(|runs| runs.lines().count())
        .ok();
    // Typed teardown through the dead guest converges directly;
    // its error (not a fabricated outcome) is the report.
    let teardown_result = holder.get().teardown_unit(
        &unit,
        Duration::from_secs(10),
        &key,
        &container,
        &session_id,
        false,
    );
    let rerun_count = std::fs::read_to_string(&marker)
        .map(|runs| runs.lines().count())
        .ok();
    let residue_clean = residue_gone(&container, &session_id);
    let _ = holder.take().close();
    if residue_clean {
        guard.container_name = None;
        guard.session_id = None;
    }
    assert!(
        matches!(awaited, Err(cistella::error::CistellaError::Protocol(_))),
        "await after the kill fails typed"
    );
    assert_eq!(
        runs_count,
        Some(1),
        "harness ran exactly once before teardown"
    );
    assert!(
        teardown_result.is_err(),
        "teardown after launch death reports, never cleans silently"
    );
    assert_eq!(rerun_count, Some(1), "teardown launched nothing");
    assert!(residue_clean, "no residue after launch-death teardown");
}

/// Polls one readiness file with a bounded wait: a missed signal
/// retires explicitly in the caller instead of hanging the suite.
fn poll_present(path: &std::path::Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn recovery_launch_noack_never_replays() {
    // Production-path proof of the ambiguous-launch boundary: the
    // real guest spawns the harness, signals entered-pre-ack, and
    // blocks before writing the response; the test kills inside
    // that window without releasing. The launch observes death
    // with NO terminal response (typed failure, nothing
    // fabricated), the harness ran exactly once, and teardown
    // converges without any relaunch. Ownership is Arc-shaped —
    // the launch thread borrows nothing the main thread later
    // moves; the main thread reclaims the sole client only after
    // joining (kill first, so the join is bounded by dispatcher
    // death detection), closes exactly once at the end, and the
    // name-based guard stays armed until `residue_gone` proves
    // clean.
    let Some(image) = fixture_image() else { return };
    let marker_base = TempDir::new().expect("marker base tempdir");
    // SAFETY: unique variable name no other test reads; the
    // per-unit arm file gates siblings (a lingering variable
    // without an arm file leaves the hook inert, and arm files die
    // with their TempDirs on every path).
    unsafe {
        std::env::set_var(
            "CISTELLA_QA_MARKER_DIR",
            marker_base.path().to_string_lossy().to_string(),
        );
    }
    let rendezvous = TempDir::new().expect("tempdir");
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree);
    let container = session.container_name();
    let session_id = session.id.clone();
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
    };
    // Name guard older than the Arc guard: unwind reclaims/closes
    // the guest before converging by name (Drop order is reverse
    // declaration). The Arc guard reports honestly when wedged.
    let mut guard = LiveUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    // Arc guard from hosting onward: any panic before the explicit
    // reclaim still reports worker/rendezvous state through Drop
    // (reclaim-then-close only when uniquely held — a wedged clone
    // reports instead of pretending).
    let mut holder = ArcCloseGuard::new(std::sync::Arc::new(host_wire(&rendezvous)));
    let unit = holder.get().create(&spec, &key).expect("create");
    holder.get().initiate(&unit, &key).expect("initiate");
    // Arm this unit's hold: no sibling session owns an arm file
    // for its own handle, so parallel live tests never stall.
    let unit_dir = marker_base.path().join(unit.as_str());
    std::fs::create_dir_all(&unit_dir).expect("arm dir");
    std::fs::write(unit_dir.join("arm"), "hold").expect("arm file");
    let launch_key = ReconciliationKey::generate();
    let launcher = std::sync::Arc::clone(holder.get());
    let unit_moved = unit.clone();
    // Result channel proves thread completion: the main thread
    // NEVER blocking-joins a possibly wedged launch. A received
    // result (sent as the thread's last act) or a disconnected
    // sender means the closure returned and its Arc dropped; only
    // then are join/try_unwrap sound. A channel timeout means
    // wedged — never join, never close, guard stays armed.
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let launch_thread = std::thread::spawn(move || {
        let outcome = launcher.execute_launch(
            &unit_moved,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo run >> /work/runs; sleep 30".to_string(),
            ],
            Some("/work"),
            StdioBinding::Inherit,
            &launch_key,
        );
        let _ = result_tx.send(outcome);
    });
    // Both readiness facts precede the kill: the guest reached its
    // hold (spawn proven host-side) and the harness ran (proven
    // container-side). Bounded waits; a miss retires explicitly.
    let runs_marker = worktree.path().join("runs");
    let spawned = poll_present(&unit_dir.join("entered-pre-ack")) && poll_present(&runs_marker);
    // The kill releases the blocked launch either way. Reclaim
    // only on channel proof below — never a blind join.
    kill_guest(&rendezvous);
    wait_death_observed(holder.get());
    let launched = match result_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(outcome) => outcome,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("launch thread ended without a result; worker state unknown, unit guard armed");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Wedged: the handle drops here (detach, documented),
            // never joined; the client is never closed; both guards
            // report through their Drops (ref count + name-based
            // converge).
            panic!(
                "launch thread wedged after kill (client refs held: {}); worker/guest/thread \
                 residue possible, unit guard armed for name-based converge",
                std::sync::Arc::strong_count(holder.get()),
            );
        }
    };
    // Proven complete: joining returns promptly and drops the
    // thread's Arc, so the sole client is reclaimable after.
    launch_thread.join().expect("completed launcher joins");
    if !spawned {
        // Readiness missed: converge explicitly, reclaim, close,
        // then fail loudly — never claim clean.
        let _ = holder.get().teardown_unit(
            &unit,
            Duration::from_secs(10),
            &key,
            &container,
            &session_id,
            false,
        );
        let owned = match std::sync::Arc::try_unwrap(holder.take()) {
            Ok(owned) => owned,
            Err(_) => panic!("launcher joined; sole client ref expected"),
        };
        let _ = owned.close();
        panic!("spawn was never proven inside the hold window");
    }
    // Capture everything into locals BEFORE any failure
    // assertion (no `expect_err`/`expect` on the regression
    // values): teardown and close run first, the name-based guard
    // disarms only on verified clean, and only then do the asserts
    // run. A regression (unexpected ACK, double run) fails AFTER
    // cleanup, never during it.
    let launched_err = launched.err();
    let runs_count = std::fs::read_to_string(&runs_marker)
        .map(|runs| runs.lines().count())
        .ok();
    // Typed teardown through the dead guest: converges without any
    // relaunch (the second count below would catch a second run).
    // Borrowed through the guard; reclaim comes after.
    let teardown_result = holder.get().teardown_unit(
        &unit,
        Duration::from_secs(10),
        &key,
        &container,
        &session_id,
        false,
    );
    let rerun_count = std::fs::read_to_string(&runs_marker)
        .map(|runs| runs.lines().count())
        .ok();
    let residue_clean = residue_gone(&container, &session_id);
    // Reclaim only after the joined thread dropped its Arc;
    // teardown above already ran through the shared borrow.
    let owned = match std::sync::Arc::try_unwrap(holder.take()) {
        Ok(owned) => owned,
        Err(_) => panic!("launcher joined; sole client ref expected"),
    };
    let _ = owned.close();
    if residue_clean {
        guard.container_name = None;
        guard.session_id = None;
    }
    assert!(
        matches!(
            launched_err,
            Some(cistella::error::CistellaError::Protocol(_))
        ),
        "ambiguous launch fails typed"
    );
    assert_eq!(runs_count, Some(1), "harness ran exactly once");
    assert!(
        teardown_result.is_err(),
        "teardown after no-ack death reports, never cleans silently"
    );
    assert_eq!(rerun_count, Some(1), "teardown launched nothing");
    assert!(residue_clean, "no residue after no-ack teardown");
    // SAFETY: hook is inert without arm files (removed with their
    // TempDirs above); no sibling depends on this name.
    unsafe {
        std::env::remove_var("CISTELLA_QA_MARKER_DIR");
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn recovery_conduct_rehosts_through_guest_kill() {
    // Full conduct episode under kill: the start-delay hook opens a
    // deterministic window between install and start (unit file
    // present, no initiate sent). SIGKILLing the guest there forces
    // conduct's pre-exec episode to retire, re-host, adopt by key,
    // and complete — exit 0 with the harness outcome, residue free.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let fixture = fixture_profile("default.toml");
    let unit_dir = PathBuf::from(&home).join(".config/containers/systemd");
    let before: std::collections::HashSet<String> = std::fs::read_dir(&unit_dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();

    let mut child = Command::new(bin())
        .args([
            "conduct",
            "--profile",
            fixture.as_str(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "true",
        ])
        .env("HOME", &home)
        .env("TERM", "xterm-ghostty")
        .env("CISTELLA_CONDUCT_START_DELAY_MS", "8000")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn conduct");
    // The installed unit file marks the delay window: create is
    // done, initiate unsent — the kill lands pre-exec by
    // construction, not by timing.
    let (id, upath) = wait_for_unit_file(&unit_dir, &before, &worktree_str);
    let mut guard = Guard {
        id: Some(id.clone()),
    };
    kill_guest_child_of(child.id());
    let status = child.wait().expect("conduct reaped");
    assert_eq!(status.code(), Some(0), "conduct recovers and exits 0");
    assert!(!upath.exists(), "adopted unit removed after recovery");
    assert!(scratch_gone(&id), "scratch removed after recovery");
    guard.id = None;
}
