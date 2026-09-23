//! Podman lifecycle conformance through the framework trait.
//!
//! The real-lifecycle half of the conformance suite (spec
//! `isolator-contract`): create → initiate → execute → inspect/state
//! → terminate → remove against the live backend, with env/mount
//! fidelity, idempotent teardown, and residue freedom. Fast-half
//! trait logic lives in `tests/unit/isolator_trait.rs`; the
//! deterministic protocol peer (task 3.1) proves boundary faults.

use std::time::{Duration, Instant};

use tempfile::TempDir;

use cistella::framework::contract::{Capability, ReconciliationKey};
use cistella::framework::isolator::{CreateSpec, ExecutionOutcome, Isolator, StdioBinding};
use cistella::isolators::podman::PodmanIsolator;
use cistella::isolators::quadlet::{residue_gone, resolve_image_digest};
use cistella::mount::{MountMode, MountTriple, podman_volume_args};
use cistella::session::{Session, mint_session_id};

use super::helpers::*;

const FIXTURE_IMAGE: &str = "localhost/cistella/opencode:example";

/// Live backend under test plus converging guard.
struct Fixture {
    backend: PodmanIsolator,
    key: ReconciliationKey,
    handle: Option<cistella::framework::contract::UnitHandle>,
    container: String,
    session_id: String,
}

impl Fixture {
    fn teardown(&mut self) {
        if let Some(handle) = self.handle.take() {
            let grace = Duration::from_secs(10);
            let _ = self.backend.terminate(&handle, grace, &self.key);
            let _ = self.backend.remove(&handle, &self.key);
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Resolves the fixture image or skips when the host lacks it.
fn fixture_image() -> Option<String> {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return None;
    }
    match resolve_image_digest(FIXTURE_IMAGE) {
        Ok(image) => Some(image),
        Err(error) => {
            eprintln!("skip: fixture image unavailable: {error}");
            None
        }
    }
}

/// Builds a session plus volumes over a worktree tempdir (mirrors the
/// conduct planning the prepare transaction will centralize in 2.2).
fn plan_session(image: &str, worktree: &TempDir, marker: &str) -> (Session, Vec<String>) {
    let id = mint_session_id();
    let directory = worktree.path().to_string_lossy().to_string();
    std::fs::write(worktree.path().join(marker), "marker").expect("write marker");
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

/// Creates and initiates a unit, returning the fixture.
fn launch_unit(image: &str, env: Vec<String>) -> (Fixture, TempDir) {
    let backend = PodmanIsolator::new();
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(image, &worktree, "marker");
    let container = session.container_name();
    let session_id = session.id.clone();
    let spec = CreateSpec {
        session,
        volumes,
        env,
        labels: vec![],
    };
    let handle = backend.create(&spec, &key).expect("create unit");
    assert_eq!(backend.locate(&key), Some(handle.clone()));
    assert_eq!(
        backend.state(&handle).expect("state after create"),
        cistella::framework::contract::LifecycleState::Created
    );
    let attestation = backend.initiate(&handle, &key).expect("initiate unit");
    assert!(attestation.ready);
    assert_eq!(attestation.unit_identity, container);
    assert_eq!(
        backend.state(&handle).expect("state after initiate"),
        cistella::framework::contract::LifecycleState::Initiated
    );
    (
        Fixture {
            backend,
            key,
            handle: Some(handle),
            container,
            session_id,
        },
        worktree,
    )
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conformance_full_cycle_with_fidelity() {
    let Some(image) = fixture_image() else { return };
    // Podman realizes the contribution types conduct routes today.
    let capabilities = PodmanIsolator::new().capabilities();
    assert!(capabilities.supports(Capability::Environment));
    assert!(capabilities.supports(Capability::Mounts));
    let env = vec!["CONFORMANCE_PROBE=cistella-conformance".to_string()];
    let (mut fixture, _worktree) = launch_unit(&image, env);
    let handle = fixture.handle.clone().expect("unit handle");
    let key = ReconciliationKey::generate();

    // Env fidelity: the harness exfils the contribution via scratch.
    let probe = fixture
        .backend
        .execute_launch(
            &handle,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo \"$CONFORMANCE_PROBE\" > /tmp/scratch/probe".to_string(),
            ],
            Some("/work"),
            StdioBinding::Inherit,
            &key,
        )
        .expect("launch probe");
    assert_eq!(
        fixture
            .backend
            .await_result(
                &probe,
                &cistella::framework::contract::CancelFlag::default()
            )
            .expect("await probe"),
        ExecutionOutcome::Exited(0)
    );
    let scratch_file = cistella::lock::scratch_dir(&fixture.session_id).join("probe");
    let contents = std::fs::read_to_string(&scratch_file).expect("read probe");
    assert_eq!(contents, "cistella-conformance\n");

    // Mount fidelity: the worktree marker is visible at /work.
    let mount = fixture
        .backend
        .execute_launch(
            &handle,
            &[
                "test".to_string(),
                "-f".to_string(),
                "/work/marker".to_string(),
            ],
            Some("/work"),
            StdioBinding::Inherit,
            &key,
        )
        .expect("launch mount check");
    assert_eq!(
        fixture
            .backend
            .await_result(
                &mount,
                &cistella::framework::contract::CancelFlag::default()
            )
            .expect("await mount check"),
        ExecutionOutcome::Exited(0)
    );

    // Exit codes pass through; inspect pins the snapshot shape.
    let failing = fixture
        .backend
        .execute_launch(
            &handle,
            &["false".to_string()],
            Some("/work"),
            StdioBinding::Inherit,
            &key,
        )
        .expect("launch false");
    assert_eq!(
        fixture
            .backend
            .await_result(
                &failing,
                &cistella::framework::contract::CancelFlag::default()
            )
            .expect("await false"),
        ExecutionOutcome::Exited(1)
    );
    let snapshot = fixture.backend.inspect(&handle).expect("inspect unit");
    assert_eq!(snapshot.unit_identity, fixture.container);
    assert_eq!(snapshot.session_id, fixture.session_id);

    // Idempotent teardown: terminate/remove twice succeed.
    let grace = Duration::from_secs(10);
    fixture
        .backend
        .terminate(&handle, grace, &key)
        .expect("terminate");
    fixture
        .backend
        .terminate(&handle, grace, &key)
        .expect("re-terminate");
    fixture.backend.remove(&handle, &key).expect("remove");
    fixture.backend.remove(&handle, &key).expect("re-remove");
    assert_eq!(
        fixture.backend.state(&handle).expect("state after remove"),
        cistella::framework::contract::LifecycleState::Absent
    );
    fixture.handle = None;
    assert!(
        residue_gone(&fixture.container, &fixture.session_id),
        "teardown leaves no residue"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conformance_executing_state_observable() {
    let Some(image) = fixture_image() else { return };
    let (fixture, _worktree) = launch_unit(&image, vec![]);
    let handle = fixture.handle.clone().expect("unit handle");
    let key = ReconciliationKey::generate();
    let sleep = fixture
        .backend
        .execute_launch(
            &handle,
            &["sleep".to_string(), "30".to_string()],
            Some("/work"),
            StdioBinding::Inherit,
            &key,
        )
        .expect("launch sleep");
    // Poll: the container needs a moment to reach running.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = fixture.backend.state(&handle).expect("state during exec");
        if state == cistella::framework::contract::LifecycleState::Executing {
            break;
        }
        if Instant::now() > deadline {
            panic!("sleep never observed executing");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let cancel = cistella::framework::contract::CancelFlag::default();
    cancel.cancel_with(15);
    assert_eq!(
        fixture
            .backend
            .await_result(&sleep, &cancel)
            .expect("await sleep"),
        ExecutionOutcome::Signaled(15)
    );
}
