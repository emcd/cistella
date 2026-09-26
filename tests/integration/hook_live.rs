//! Hooked-launch live proofs (task 3.2 chain, QA seat).
//!
//! Staging, guest-context probe, diagnostics forwarding, apply,
//! and attestation through the real guest: a hooked unit confines
//! its harness to the project subtree while the ancestor denies.
//! Runs only on the operator-authorized seat (systemd user
//! manager, podman, crun runtime, fixture image).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;

use cistella::framework::contract::{CancelFlag, LifecycleState, ReconciliationKey};
use cistella::framework::contract::{GuestHookRequest, HookArtifact, HookProbe, HookSource};
use cistella::framework::isolator::{CreateSpec, ExecutionOutcome, Isolator, StdioBinding};
use cistella::framework::prepare::{compose_hook_argv, parse_attestation_line, parse_probe_report};
use cistella::framework::registry::{
    STAGED_WRAPPER_GUEST_PATH, WRAPPER_FILE_NAME, digest_sibling, stage_hook_artifact,
};
use cistella::isolators::client::{ISOLATOR_BIN, WireClient};
use cistella::mount::{MountMode, MountTriple, podman_volume_args};
use cistella::session::{Session, mint_session_id};

use super::helpers::*;
use super::protocol_peer::peer_path;

const FIXTURE_IMAGE: &str = "localhost/cistella/opencode:example";

/// Bins directory: profile dir above the examples dir.
fn bins_dir() -> PathBuf {
    peer_path()
        .parent()
        .expect("examples dir")
        .parent()
        .expect("profile dir")
        .to_path_buf()
}

/// Hosts the real guest binary (missing binary is an environment
/// bug, not a skip).
fn host_wire(rendezvous: &TempDir) -> WireClient {
    let dir = bins_dir();
    assert!(
        dir.join(ISOLATOR_BIN).exists(),
        "guest binary missing: run `cargo build --bin {ISOLATOR_BIN}` first"
    );
    assert!(
        dir.join(WRAPPER_FILE_NAME).exists(),
        "wrapper binary missing: run `cargo build --bin {WRAPPER_FILE_NAME}` first"
    );
    WireClient::host(&dir, rendezvous.path(), Default::default()).expect("host real guest")
}

/// Best-effort name-based converge for hook-created units (no
/// handle record exists outside the test). Armed until disarmed.
struct HookUnitGuard {
    container_name: Option<String>,
    session_id: Option<String>,
}

impl Drop for HookUnitGuard {
    fn drop(&mut self) {
        if let (Some(container), Some(session)) =
            (self.container_name.take(), self.session_id.take())
        {
            let _ = cistella::runtime::teardown(&container, &session);
        }
    }
}

/// Hooked fixture: ancestor tree `T` mounted broad-RW at `/src`,
/// project subtree `T/proj`, sibling `T/sib`, staged wrapper RO.
/// Declaration order is the unwind order (reversed): name guard
/// older, client guard newer.
struct HookFixture {
    _rendezvous: TempDir,
    _tree: TempDir,
    _staged: cistella::framework::registry::StagedHook,
    client: Option<WireClient>,
    key: ReconciliationKey,
    handle: cistella::framework::contract::UnitHandle,
    container: String,
    triples: Vec<MountTriple>,
    proj: PathBuf,
    #[allow(dead_code)]
    guard: HookUnitGuard,
}

fn hook_fixture(image: &str) -> HookFixture {
    let rendezvous = TempDir::new().expect("rendezvous tempdir");
    let tree = TempDir::new().expect("tree tempdir");
    let proj = tree.path().join("proj");
    std::fs::create_dir_all(&proj).expect("proj dir");
    std::fs::create_dir_all(tree.path().join("sib")).expect("sib dir");
    std::fs::write(proj.join("seed"), "seed").expect("seed marker");
    let id = mint_session_id();
    // Stage exactly as the extension answers: observe the shipped
    // digest, then stage the admitted bytes.
    let exe = bins_dir();
    let sha = digest_sibling(&exe, WRAPPER_FILE_NAME).expect("wrapper digest");
    let artifact = HookArtifact {
        kind: "digest-pinned-blob".to_string(),
        sha256: sha,
        source: HookSource {
            registry: "shipped".to_string(),
            path: WRAPPER_FILE_NAME.to_string(),
        },
    };
    let (staged, staged_triple) =
        stage_hook_artifact(&exe, &id, 0, &artifact).expect("stage wrapper");
    let session = Session {
        id: id.clone(),
        directory: proj.to_string_lossy().to_string(),
        profile: "conformance".to_string(),
        profile_digest: "conformance-test".to_string(),
        identity: "conformance".to_string(),
        command: vec!["sleep".to_string(), "infinity".to_string()],
        image: image.to_string(),
        container_home: "/home/cistella".to_string(),
    };
    let container = session.container_name();
    let session_id = session.id.clone();
    let scratch = cistella::lock::scratch_dir(&id)
        .to_string_lossy()
        .to_string();
    let triples = vec![
        // The broad mount declares RO (the 0.2 goal): revision
        // converts it to RW for Podman while the Landlock policy
        // enforces read-only with the subtree carveout.
        MountTriple {
            host_source: tree.path().to_string_lossy().to_string(),
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: scratch,
            container_target: "/tmp/scratch".to_string(),
            mode: MountMode::Rw,
        },
        staged_triple,
    ];
    let revised = cistella::mount::revise_ro_for_confinement(&triples);
    let volumes = podman_volume_args(&revised, &session.container_home.clone(), None);
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
    };
    let guard = HookUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    let mut holder = CloseGuard::new(host_wire(&rendezvous));
    let client = holder.get();
    let key = ReconciliationKey::generate();
    let handle = client.create(&spec, &key).expect("wire create");
    assert_eq!(
        client.state(&handle).expect("state after create"),
        LifecycleState::Created
    );
    let attestation = client.initiate(&handle, &key).expect("wire initiate");
    assert!(attestation.ready, "initiate must report ready");
    let client = holder.take();
    HookFixture {
        _rendezvous: rendezvous,
        _tree: tree,
        _staged: staged,
        client: Some(client),
        key,
        handle,
        container,
        triples,
        proj,
        guard,
    }
}

impl HookFixture {
    fn client(&self) -> &WireClient {
        self.client.as_ref().expect("client live")
    }

    fn teardown(&mut self) {
        let client = self.client.take().expect("client live");
        let _ = client.terminate(&self.handle, Default::default(), &self.key);
        let _ = client.remove(&self.handle, &self.key);
        let _ = client.close();
    }
}

/// Runs `podman exec <container> <argv...>` with piped stdio.
fn podman_exec(container: &str, argv: &[&str]) -> std::process::Output {
    std::process::Command::new("podman")
        .arg("exec")
        .arg(container)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("podman exec must spawn")
}

/// Reads one attestation line from a diagnostics read-end (test
/// copy of the conduct gate: bounded wait, first line only).
fn test_attestation(read: &std::os::fd::OwnedFd) -> String {
    use cistella::framework::prepare::read_attestation_line;
    use std::os::fd::AsFd;
    let (line, _) =
        read_attestation_line(read.as_fd(), Duration::from_secs(30)).expect("attestation line");
    line
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn hook_probe_reports_matrix_in_container() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let Some(image) = fixture_image_opt() else {
        return;
    };
    let mut fixture = hook_fixture(&image);
    let out = podman_exec(&fixture.container, &[STAGED_WRAPPER_GUEST_PATH, "--probe"]);
    assert!(out.status.success(), "probe must exit 0 in-container");
    parse_probe_report(&out.stdout).expect("probe matrix gates");
    fixture.teardown();
}

/// Hooked launch attests applied and confines: admitted write
/// succeeds, sibling write fails, both under one attestation.
#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn hook_hooked_launch_attests_and_confines() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let Some(image) = fixture_image_opt() else {
        return;
    };
    let mut fixture = hook_fixture(&image);
    let ancestor = cistella::mount::guest_routes_for_host(
        &fixture.triples,
        fixture.proj.parent().expect("tree"),
    );
    assert_eq!(ancestor, vec!["/src".to_string()]);
    let subtree = cistella::mount::guest_routes_for_host(&fixture.triples, &fixture.proj);
    assert_eq!(subtree, vec!["/src/proj".to_string()]);
    // Admitted harness: marker inside the subtree, argv through
    // the real derivation (baseline included).
    let admitted = hooked_argv(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo ok > /src/proj/marker".to_string(),
        ],
    );
    let (diag_read, diag_write) =
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).expect("diagnostics pipe");
    let execution = {
        use std::os::fd::AsFd;
        fixture
            .client()
            .execute_launch_hooked(
                &fixture.handle,
                &admitted,
                Some("/src/proj"),
                StdioBinding::Inherit,
                &fixture.key,
                diag_write.as_fd(),
            )
            .expect("hooked launch")
    };
    drop(diag_write);
    let line = test_attestation(&diag_read);
    let abi = parse_attestation_line(&line).expect("applied attestation");
    assert!(abi >= 3, "attested ABI carries TRUNCATE");
    drop(diag_read);
    let outcome = fixture
        .client()
        .await_result(&execution, &CancelFlag::default())
        .expect("await admitted");
    assert!(
        matches!(outcome, ExecutionOutcome::Exited(0)),
        "admitted harness must exit 0, got {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.proj.join("marker"))
            .expect("marker readable")
            .as_str(),
        "ok\n"
    );
    // Denied harness: sibling write fails, attestation still applied.
    let denied = hooked_argv(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo escape > /src/sib/escape".to_string(),
        ],
    );
    let (diag_read, diag_write) =
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).expect("diagnostics pipe");
    let execution = {
        use std::os::fd::AsFd;
        fixture
            .client()
            .execute_launch_hooked(
                &fixture.handle,
                &denied,
                Some("/src/proj"),
                StdioBinding::Inherit,
                &fixture.key,
                diag_write.as_fd(),
            )
            .expect("hooked launch")
    };
    drop(diag_write);
    let line = test_attestation(&diag_read);
    parse_attestation_line(&line).expect("applied attestation");
    drop(diag_read);
    let outcome = fixture
        .client()
        .await_result(&execution, &CancelFlag::default())
        .expect("await denied");
    assert!(
        !matches!(outcome, ExecutionOutcome::Exited(0)),
        "denied harness must fail, got {outcome:?}"
    );
    assert!(
        !fixture
            .proj
            .parent()
            .expect("tree")
            .join("sib/escape")
            .exists(),
        "denied file must not exist"
    );
    fixture.teardown();
}

/// Composes the hooked launch argv through the real derivation
/// (system baseline, ancestor, subtree, session mounts): the live
/// test exercises the same argv the conductor builds.
fn hooked_argv(fixture: &HookFixture, harness: &[String]) -> Vec<String> {
    let hook = GuestHookRequest {
        artifact: HookArtifact {
            kind: "digest-pinned-blob".to_string(),
            sha256: "unused-live".to_string(),
            source: HookSource {
                registry: "shipped".to_string(),
                path: "cistella-landlock-wrap".to_string(),
            },
        },
        staging: "isolator-staged".to_string(),
        order: 0,
        argv_prefix: vec![STAGED_WRAPPER_GUEST_PATH.to_string()],
        probe: HookProbe {
            op: "probe_capabilities".to_string(),
            timeout_ms: 10_000,
        },
        on_failure: "fail-pre-exec".to_string(),
    };
    compose_hook_argv(
        &[hook],
        &fixture.triples,
        fixture.proj.parent().expect("tree"),
        &fixture.proj,
        harness,
    )
    .expect("compose hooked argv")
}

/// Fixture image, skipped quietly when unresolvable (same shape as
/// Fixture image, skipped quietly when unresolvable (same shape as
/// the parity suite's image gate).
fn fixture_image_opt() -> Option<String> {
    use cistella::runtime::resolve_image_digest;
    match resolve_image_digest(FIXTURE_IMAGE) {
        Ok(image) => Some(image),
        Err(error) => {
            eprintln!("skip: fixture image unavailable: {error}");
            None
        }
    }
}
