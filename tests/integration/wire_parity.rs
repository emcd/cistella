//! Wire/reference parity: the same lifecycle through both backends.
//!
//! The in-process `PodmanIsolator` is the conformance reference;
//! production conduct drives the external guest through
//! `WireClient`. These mirrors run each reference scenario from
//! `conformance.rs` against the wire backend and assert the same
//! observable outcomes (lifecycle states, attestations, exit
//! codes, residue freedom — never handle strings, which each
//! backend mints independently). The transcript test runs one
//! shared driver against both backends and diffs normalized
//! evidence, so any behavioral divergence fails the suite by name.

use std::time::{Duration, Instant};

use tempfile::TempDir;

use cistella::framework::contract::{CancelFlag, Capability, LifecycleState, ReconciliationKey};
use cistella::framework::isolator::{CreateSpec, ExecutionOutcome, Isolator, StdioBinding};
use cistella::isolators::client::{ISOLATOR_BIN, WireClient};
use cistella::isolators::podman::PodmanIsolator;
use cistella::isolators::quadlet::{residue_gone, resolve_image_digest};
use cistella::mount::{MountMode, MountTriple, podman_volume_args};
use cistella::session::{Session, mint_session_id};

use super::helpers::*;
use super::protocol_peer::peer_path;

const FIXTURE_IMAGE: &str = "localhost/cistella/opencode:example";

/// Directory holding built binaries (`target/<profile>/`): the
/// guest binary ships beside the scripted peer.
fn bins_dir() -> std::path::PathBuf {
    peer_path()
        .parent()
        .expect("examples dir")
        .parent()
        .expect("profile dir")
        .to_path_buf()
}

/// Hosts the real guest binary for one wire test (missing binary
/// is an environment bug, not a skip).
fn host_wire(rendezvous: &TempDir) -> WireClient {
    let dir = bins_dir();
    let name = ISOLATOR_BIN;
    assert!(
        dir.join(name).exists(),
        "guest binary missing: run `cargo build --bin {name}` first"
    );
    WireClient::host(&dir, rendezvous.path(), Default::default()).expect("host real guest")
}

/// Best-effort name-based converge for wire-created units (no
/// handle record exists outside the test). Armed until disarmed.
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

/// Builds a session plus volumes over a worktree tempdir (same
/// shape as the reference conformance planner).
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

/// Creates and initiates a unit on any backend, returning the
/// wire client (when used), key, handle, and guards.
struct WireFixture {
    _rendezvous: TempDir,
    _worktree: TempDir,
    client: Option<WireClient>,
    key: ReconciliationKey,
    handle: Option<cistella::framework::contract::UnitHandle>,
    container: String,
    session_id: String,
    guard: LiveUnitGuard,
}

impl WireFixture {
    fn teardown(&mut self) {
        // Disarm ONLY on wire-Ok PLUS verified absence, decided by
        // the pinned helper: nominal Ok can still leave residue,
        // and this runs during Drop. Anything else keeps the
        // name-based guard armed for the Drop fallback.
        if let Some(handle) = self.handle.take() {
            let client = self.client.as_ref().expect("wire client held");
            let grace = Duration::from_secs(10);
            let wire_ok = client.terminate(&handle, grace, &self.key).is_ok()
                && client.remove(&handle, &self.key).is_ok();
            if teardown_disarms(wire_ok, residue_gone(&self.container, &self.session_id)) {
                self.guard.container_name = None;
                self.guard.session_id = None;
            }
        }
    }
}

/// Disarm decision for the fixture teardown above, pinned
/// directly AND used at the real callsite: wire-Ok plus verified
/// absence disarms; anything else retains the name-based fallback
/// (even nominal Ok can leave residue, and this runs during Drop).
fn teardown_disarms(wire_ok: bool, residue_ok: bool) -> bool {
    wire_ok && residue_ok
}

#[test]
fn wire_fixture_teardown_disarms_only_on_verified_clean() {
    assert!(teardown_disarms(true, true));
    assert!(!teardown_disarms(false, true));
    assert!(!teardown_disarms(true, false));
    assert!(!teardown_disarms(false, false));
}

impl Drop for WireFixture {
    fn drop(&mut self) {
        self.teardown();
        if let Some(client) = self.client.take() {
            let _ = client.close();
        }
    }
}

/// Launches a unit through the wire backend (mirror of the
/// reference `launch_unit`).
fn launch_wire_unit(image: &str, env: Vec<String>) -> WireFixture {
    let rendezvous = TempDir::new().expect("rendezvous tempdir");
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
    // Declaration order IS the unwind order (reversed): the name
    // guard is older, so on panic the client closes/quiesces first
    // and name teardown runs second — a late install can never
    // escape the residue check beside a live guest.
    let fixture_guard = LiveUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    // Close-on-unwind from hosting onward: any panic before the
    // fixture takes the client still joins the worker and unlinks
    // rendezvous through Drop.
    let mut holder = CloseGuard::new(host_wire(&rendezvous));
    let client = holder.get();
    let handle = client.create(&spec, &key).expect("wire create");
    assert_eq!(
        client.state(&handle).expect("state after create"),
        LifecycleState::Created
    );
    let attestation = client.initiate(&handle, &key).expect("wire initiate");
    assert!(attestation.ready);
    assert_eq!(attestation.unit_identity, container);
    assert_eq!(
        client.state(&handle).expect("state after initiate"),
        LifecycleState::Initiated
    );
    // Fixture takes both guards from here: the name-based guard
    // for unit/scratch, the close guard's client for its Drop.
    let client = holder.take();
    WireFixture {
        _rendezvous: rendezvous,
        _worktree: worktree,
        client: Some(client),
        key,
        handle: Some(handle),
        container: container.clone(),
        session_id: session_id.clone(),
        guard: fixture_guard,
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn wire_parity_full_cycle_with_fidelity() {
    let Some(image) = fixture_image() else { return };
    // Same capabilities the reference asserts: the wire client
    // realizes exactly what its guest does.
    let rendezvous = TempDir::new().expect("tempdir");
    let mut probe = CloseGuard::new(host_wire(&rendezvous));
    let capabilities = probe.get().capabilities();
    assert!(capabilities.supports(Capability::Environment));
    assert!(capabilities.supports(Capability::Mounts));
    let _ = probe.take().close();
    let env = vec!["CONFORMANCE_PROBE=cistella-conformance".to_string()];
    let mut fixture = launch_wire_unit(&image, env);
    let client = fixture.client.as_ref().expect("wire client");
    let handle = fixture.handle.clone().expect("unit handle");
    let key = ReconciliationKey::generate();

    // Env fidelity: the harness exfils the contribution via scratch.
    let probe = client
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
        client
            .await_result(&probe, &CancelFlag::default())
            .expect("await probe"),
        ExecutionOutcome::Exited(0)
    );
    let scratch_file = cistella::lock::scratch_dir(&fixture.session_id).join("probe");
    let contents = std::fs::read_to_string(&scratch_file).expect("read probe");
    assert_eq!(contents, "cistella-conformance\n");

    // Mount fidelity: the worktree marker is visible at /work.
    let mount = client
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
        client
            .await_result(&mount, &CancelFlag::default())
            .expect("await mount check"),
        ExecutionOutcome::Exited(0)
    );

    // Exit codes pass through; inspect pins the snapshot shape.
    let failing = client
        .execute_launch(
            &handle,
            &["false".to_string()],
            Some("/work"),
            StdioBinding::Inherit,
            &key,
        )
        .expect("launch false");
    assert_eq!(
        client
            .await_result(&failing, &CancelFlag::default())
            .expect("await false"),
        ExecutionOutcome::Exited(1)
    );
    let snapshot = client.inspect(&handle).expect("inspect unit");
    assert_eq!(snapshot.unit_identity, fixture.container);
    assert_eq!(snapshot.session_id, fixture.session_id);

    // Teardown divergence (designed, pinned per side): the guest
    // evicts the framework binding on remove, so post-remove ops
    // refuse unknown-handle; the reference retains records and
    // stays idempotent. Both converge; only the handle lifetime
    // differs.
    let grace = Duration::from_secs(10);
    client.terminate(&handle, grace, &key).expect("terminate");
    client.remove(&handle, &key).expect("remove");
    let error = client
        .terminate(&handle, grace, &key)
        .expect_err("wire re-terminate refuses evicted binding");
    assert!(
        matches!(error, cistella::error::CistellaError::Contract(_)),
        "typed unknown-handle refusal, got: {error}"
    );
    let error = client
        .remove(&handle, &key)
        .expect_err("wire re-remove refuses evicted binding");
    assert!(
        matches!(error, cistella::error::CistellaError::Contract(_)),
        "typed unknown-handle refusal, got: {error}"
    );
    let error = client
        .state(&handle)
        .expect_err("wire post-remove state refuses evicted binding");
    assert!(
        matches!(error, cistella::error::CistellaError::Contract(_)),
        "typed unknown-handle refusal, got: {error}"
    );
    fixture.handle = None;
    assert!(
        residue_gone(&fixture.container, &fixture.session_id),
        "teardown leaves no residue"
    );
    fixture.guard.container_name = None;
    fixture.guard.session_id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn wire_parity_reconciliation_survives_replacement() {
    // Mirror of the reference crash-after-install proof at the wire
    // layer: a FRESH client with empty tables presents the same key
    // and adopts the surviving unit (new handle, same identity),
    // then converges through it. This is the primitive conduct's
    // pre-exec episode composes.
    let Some(image) = fixture_image() else { return };
    let rendezvous = TempDir::new().expect("tempdir");
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree, "marker");
    let container = session.container_name();
    let session_id = session.id.clone();
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
    };
    // Name guard older than the client guard: unwind closes the
    // guest before converging by name (see `launch_wire_unit`).
    let mut guard = LiveUnitGuard {
        container_name: Some(container.clone()),
        session_id: Some(session_id.clone()),
    };
    let mut first = CloseGuard::new(host_wire(&rendezvous));
    let created = first.get().create(&spec, &key).expect("wire create");
    // Replacement client while the first guest still lives: locate
    // is durable-state based, so the same key adopts with no death
    // involved (the crash variant lives in recovery.rs). The first
    // client closes only after the replacement converges, so the
    // unit survives the switch.
    let rendezvous2 = TempDir::new().expect("tempdir");
    let mut second = CloseGuard::new(host_wire(&rendezvous2));
    let adopted = second
        .get()
        .create(&spec, &key)
        .expect("same-key retry adopts");
    assert_ne!(
        adopted.as_str(),
        created.as_str(),
        "adopt mints a new framework handle"
    );
    assert_eq!(
        second
            .get()
            .inspect(&adopted)
            .expect("inspect adopted")
            .unit_identity,
        container
    );
    second
        .get()
        .initiate(&adopted, &key)
        .expect("initiate adopted");
    assert_eq!(
        second.get().state(&adopted).expect("state after initiate"),
        LifecycleState::Initiated
    );
    second
        .get()
        .converge_clean(&adopted, Duration::from_secs(10), &key)
        .expect("converge adopted");
    assert!(
        residue_gone(&container, &session_id),
        "converged unit leaves no residue"
    );
    guard.container_name = None;
    guard.session_id = None;
    let _ = second.take().close();
    // First closes last: its converge sweep finds nothing left and
    // stays clean (idempotent backend teardown).
    let _ = first.take().close();
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn wire_parity_absent_converge_clears_orphan_scratch() {
    let Some(image) = fixture_image() else { return };
    let mut fixture = launch_wire_unit(&image, vec![]);
    let client = fixture.client.as_ref().expect("wire client");
    let handle = fixture.handle.clone().expect("unit handle");
    let key = ReconciliationKey::generate();
    let grace = Duration::from_secs(10);
    // Converge once from live: terminate inside converge plus
    // remove clear the unit, and the absent-with-scratch tail
    // proves `remove` owns scratch (a pre-remove here would evict
    // the binding and the wire converge could no longer run —
    // designed divergence from the reference, see full-cycle).
    client.terminate(&handle, grace, &key).expect("terminate");
    let scratch = cistella::lock::scratch_dir(&fixture.session_id);
    std::fs::create_dir_all(&scratch).expect("plant orphan scratch");
    client
        .converge_clean(&handle, grace, &key)
        .expect("converge clears scratch");
    assert!(
        residue_gone(&fixture.container, &fixture.session_id),
        "orphan scratch cleared"
    );
    fixture.handle = None;
    fixture.guard.container_name = None;
    fixture.guard.session_id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn wire_parity_executing_state_observable() {
    let Some(image) = fixture_image() else { return };
    let mut fixture = launch_wire_unit(&image, vec![]);
    let client = fixture.client.as_ref().expect("wire client");
    let handle = fixture.handle.clone().expect("unit handle");
    let key = ReconciliationKey::generate();
    let sleep = client
        .execute_launch(
            &handle,
            &["sleep".to_string(), "30".to_string()],
            Some("/work"),
            StdioBinding::Inherit,
            &key,
        )
        .expect("launch sleep");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = client.state(&handle).expect("state during exec");
        if state == LifecycleState::Executing {
            break;
        }
        if Instant::now() > deadline {
            panic!("sleep never observed executing");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let cancel = CancelFlag::default();
    cancel.cancel_with(15);
    let error = client.await_result(&sleep, &cancel).unwrap_err();
    assert!(error.to_string().contains("detached"));
    client
        .terminate(&handle, Duration::from_secs(10), &key)
        .expect("terminate owns the kill");
    client
        .remove(&handle, &key)
        .expect("remove after terminate");
    fixture.handle = None;
    assert!(
        residue_gone(&fixture.container, &fixture.session_id),
        "terminated harness leaves no residue"
    );
    fixture.guard.container_name = None;
    fixture.guard.session_id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn wire_parity_await_reattaches_and_replays() {
    let Some(image) = fixture_image() else { return };
    let fixture = launch_wire_unit(&image, vec![]);
    let client = fixture.client.as_ref().expect("wire client");
    let handle = fixture.handle.clone().expect("unit handle");
    let key = ReconciliationKey::generate();
    let sleep = client
        .execute_launch(
            &handle,
            &["sleep".to_string(), "5".to_string()],
            Some("/work"),
            StdioBinding::Inherit,
            &key,
        )
        .expect("launch sleep");
    let cancel = CancelFlag::default();
    cancel.cancel_with(15);
    client.await_result(&sleep, &cancel).unwrap_err();
    let outcome = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let live = CancelFlag::default();
                client.await_result(&sleep, &live)
            })
            .join()
            .expect("waiter joins")
    })
    .expect("re-attached await redeems");
    assert_eq!(outcome, ExecutionOutcome::Exited(0));
    let live = CancelFlag::default();
    assert_eq!(
        client.await_result(&sleep, &live).expect("replay"),
        ExecutionOutcome::Exited(0)
    );
}

/// Backend-independent lifecycle evidence: states, outcomes, and
/// self-consistency only (handles and names differ per backend).
#[derive(Debug, PartialEq)]
struct CycleEvidence {
    states: Vec<LifecycleState>,
    ready: bool,
    probe: ExecutionOutcome,
    mount: ExecutionOutcome,
    failure: ExecutionOutcome,
    consistent: bool,
}

/// One driven cycle: evidence plus the live handle and names for
/// caller-owned converge.
struct CycleOutput {
    evidence: CycleEvidence,
    handle: cistella::framework::contract::UnitHandle,
    container: String,
    session_id: String,
}

/// Drives one full cycle's mutating phase on any backend.
///
/// Returns `Err` (never panics) and owns NO cleanup: the caller
/// arms its name guard in its own frame — beside its client guard
/// in declaration order that closes the guest first — and
/// converges explicitly after this returns. An inner guard here
/// would Drop beside a live guest before the caller quiesces it.
/// Backend errors propagate as-is; evidence mismatches report as
/// typed contract errors.
fn drive_cycle(
    backend: &dyn Isolator,
    spec: CreateSpec,
    key: &ReconciliationKey,
) -> Result<CycleOutput, cistella::error::CistellaError> {
    use cistella::error::CistellaError;
    let container = spec.session.container_name();
    let session_id = spec.session.id.clone();
    let mut states = Vec::new();
    let handle = backend.create(&spec, key)?;
    states.push(backend.state(&handle)?);
    let attestation = backend.initiate(&handle, key)?;
    states.push(backend.state(&handle)?);
    let launch_key = ReconciliationKey::generate();
    let probe = backend
        .execute_launch(
            &handle,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo \"$CISTELLA_PARITY\" > /tmp/scratch/parity".to_string(),
            ],
            Some("/work"),
            StdioBinding::Inherit,
            &launch_key,
        )
        .and_then(|probe| backend.await_result(&probe, &CancelFlag::default()))?;
    let mount = backend
        .execute_launch(
            &handle,
            &[
                "test".to_string(),
                "-f".to_string(),
                "/work/marker".to_string(),
            ],
            Some("/work"),
            StdioBinding::Inherit,
            &launch_key,
        )
        .and_then(|mount| backend.await_result(&mount, &CancelFlag::default()))?;
    let failure = backend
        .execute_launch(
            &handle,
            &["false".to_string()],
            Some("/work"),
            StdioBinding::Inherit,
            &launch_key,
        )
        .and_then(|failure| backend.await_result(&failure, &CancelFlag::default()))?;
    let snapshot = backend.inspect(&handle)?;
    let consistent = snapshot.unit_identity == container && snapshot.session_id == session_id;
    let scratch_file = cistella::lock::scratch_dir(&session_id).join("parity");
    let contents = std::fs::read_to_string(&scratch_file)
        .map_err(|error| CistellaError::Runtime(format!("parity probe read: {error}")))?;
    if contents != "cistella-parity\n" {
        return Err(CistellaError::Contract("parity probe mismatch".to_string()));
    }
    Ok(CycleOutput {
        evidence: CycleEvidence {
            states,
            ready: attestation.ready,
            probe,
            mount,
            failure,
            consistent,
        },
        handle,
        container,
        session_id,
    })
}

/// Caller-owned converge for one driven cycle: terminate, remove,
/// verify, and disarm only on the pinned predicate. The caller's
/// name guard stays armed through any failure above this call.
fn converge_cycle(
    backend: &dyn Isolator,
    output: &CycleOutput,
    key: &ReconciliationKey,
    guard: &mut LiveUnitGuard,
) {
    let grace = Duration::from_secs(10);
    let wire_ok = backend.terminate(&output.handle, grace, key).is_ok()
        && backend.remove(&output.handle, key).is_ok();
    let residue_clean = residue_gone(&output.container, &output.session_id);
    if teardown_disarms(wire_ok, residue_clean) {
        guard.container_name = None;
        guard.session_id = None;
    }
    assert!(
        residue_clean,
        "cycle leaves no residue for {}",
        output.container
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn wire_parity_transcript_matches_reference() {
    // One driver, two backends: identical normalized evidence or
    // the suite names the divergence. Reference runs first so a
    // wire failure cannot be mistaken for backend pollution. Each
    // leg plans its session in this frame (names known), arms its
    // name guard BEFORE hosting any client, and converges
    // explicitly — unwind order is guest-close first, name
    // teardown second, on every path.
    let Some(image) = fixture_image() else { return };
    let env = vec!["CISTELLA_PARITY=cistella-parity".to_string()];
    // Reference leg (in-process: no guest, same shape anyway).
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree, "marker");
    let mut guard = LiveUnitGuard {
        container_name: Some(session.container_name()),
        session_id: Some(session.id.clone()),
    };
    let reference = PodmanIsolator::new();
    let spec = CreateSpec {
        session,
        volumes,
        env: env.clone(),
        labels: vec![],
    };
    let reference_output = match drive_cycle(&reference, spec, &key) {
        Ok(output) => output,
        Err(error) => panic!("reference cycle: {error}"),
    };
    assert!(
        reference_output.evidence.consistent,
        "reference self-consistent: {:?}",
        reference_output.evidence
    );
    converge_cycle(&reference, &reference_output, &key, &mut guard);
    // Wire leg: guard armed before the client exists, so unwind
    // closes the guest before converging by name.
    let key = ReconciliationKey::generate();
    let worktree = TempDir::new().expect("worktree tempdir");
    let (session, volumes) = plan_session(&image, &worktree, "marker");
    let mut guard = LiveUnitGuard {
        container_name: Some(session.container_name()),
        session_id: Some(session.id.clone()),
    };
    let rendezvous = TempDir::new().expect("tempdir");
    let mut holder = CloseGuard::new(host_wire(&rendezvous));
    let spec = CreateSpec {
        session,
        volumes,
        env,
        labels: vec![],
    };
    let wire_output = match drive_cycle(holder.get(), spec, &key) {
        Ok(output) => output,
        Err(error) => panic!("wire cycle: {error}"),
    };
    converge_cycle(holder.get(), &wire_output, &key, &mut guard);
    let _ = holder.take().close();
    assert_eq!(
        wire_output.evidence, reference_output.evidence,
        "wire/reference divergence:\n wire: {:?}\n reference: {:?}",
        wire_output.evidence, reference_output.evidence
    );
}
