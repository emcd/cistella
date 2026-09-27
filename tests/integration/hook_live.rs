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
use cistella::framework::prepare::{compose_hook_argv, full_grant_routes, parse_probe_report};
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
/// project subtree `T/proj`, sibling `T/sib` (never grafted —
/// the denial path must carry no RW alias), graft content from
/// a DISJOINT tempdir `G` at `/src/graft` (same-source grafts
/// would alias `T/sib` at the dentry layer and admit through
/// it), staged wrapper RO.
/// Declaration order is the unwind order (reversed): name guard
/// older, client guard newer.
struct HookFixture {
    _rendezvous: TempDir,
    _tree: TempDir,
    _graft_src: TempDir,
    _staged: cistella::framework::registry::StagedHook,
    client: Option<WireClient>,
    key: ReconciliationKey,
    handle: cistella::framework::contract::UnitHandle,
    container: String,
    triples: Vec<MountTriple>,
    proj: PathBuf,
    graft_src: PathBuf,
    /// Guest path of the staged denial-probe helper
    /// (`/opt/probe/deny_probe*`).
    probe: String,
    #[allow(dead_code)]
    guard: HookUnitGuard,
}

fn hook_fixture(image: &str) -> HookFixture {
    let rendezvous = TempDir::new().expect("rendezvous tempdir");
    let tree = TempDir::new().expect("tree tempdir");
    let proj = tree.path().join("proj");
    std::fs::create_dir_all(&proj).expect("proj dir");
    std::fs::create_dir_all(tree.path().join("sib")).expect("sib dir");
    // Graft content lives OUTSIDE the bound tree (disjoint
    // dentries): grafting `T/sib` itself would alias it at the
    // dentry layer — Landlock is mount-agnostic, so the graft
    // FULL would admit writes through `/src/sib` and the
    // denial run would (correctly, per declarations) succeed.
    let graft_src = TempDir::new().expect("graft tempdir");
    let graft_dir = graft_src.path().join("data");
    std::fs::create_dir_all(&graft_dir).expect("graft data dir");
    // Mountpoint placeholder for the inside-tree graft target:
    // production nested_ro_preflight requires the chain to
    // pre-exist in the RO ancestor source (Podman overmounts
    // the real content onto it). Real operators mkdir the same
    // placeholder; the fixture mirrors the contract.
    std::fs::create_dir_all(tree.path().join("graft")).expect("graft placeholder");
    // Declared-RO directory under the FULL subtree (tier-2
    // RO-under-RW alias pin): must exist on host (preflight
    // shape) so the revision retention is what denies writes.
    std::fs::create_dir_all(proj.join("ro-data")).expect("ro-data dir");
    // Declared-RO directory inside the FULL subtree bound
    // outside all FULL routes (reverse-direction alias pin):
    // its dentries are FULL through the subtree rule whatever
    // its target, so only the retained Podman read-only
    // binding denies writes.
    std::fs::create_dir_all(proj.join("secret")).expect("secret dir");
    // Seed files inside both RO aliases: the denial runs below
    // first prove readability (mount materialized — rules out
    // ENOENT false-passes) and only then prove unwritability.
    std::fs::write(proj.join("ro-data/seed"), "seed").expect("ro-data seed");
    std::fs::write(proj.join("secret/seed"), "seed").expect("secret seed");
    std::fs::write(proj.join("seed"), "seed").expect("seed marker");
    let id = mint_session_id();
    // Stage the denial-probe helper for exact-syscall
    // pre/post controls: resolve the built example binary,
    // mount its parent dir read-only (disjoint dentries —
    // target/ never aliases tree content; RO skips the
    // alias guard and composes only readability), and run
    // it by exact filename in-container.
    let probe_host = example_binary("deny_probe");
    let probe_name = probe_host
        .file_name()
        .expect("probe filename")
        .to_string_lossy()
        .to_string();
    let probe = format!("/opt/probe/{probe_name}");
    let probe_parent = probe_host.parent().expect("probe parent dir");
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
        // Declared-RO directory under the FULL subtree: revision
        // retains Podman read-only (Landlock union cannot
        // subtract the parent grant, so the VFS binding carries
        // that enforcement).
        MountTriple {
            host_source: proj.join("ro-data").to_string_lossy().to_string(),
            container_target: "/src/proj/ro-data".to_string(),
            mode: MountMode::Ro,
        },
        // Declared-RO directory inside the FULL subtree bound
        // outside all FULL routes (reverse-direction alias):
        // retained Podman read-only by FULL-backed source —
        // the subtree FULL would otherwise admit through the
        // alias whatever the target-side rule says.
        MountTriple {
            host_source: proj.join("secret").to_string_lossy().to_string(),
            container_target: "/extra/secret".to_string(),
            mode: MountMode::Ro,
        },
        // Declared-RW graft of disjoint content (tier-2 graft
        // admission pin): composes into a FULL carveout by
        // declared intent. The source MUST sit outside the
        // bound tree — grafting `T/sib` itself would dentry-alias
        // it and admit writes through `/src/sib` (Landlock is
        // mount-agnostic; the product refuses such topologies
        // pre-create, see graft_alias_preflight).
        MountTriple {
            host_source: graft_dir.to_string_lossy().to_string(),
            container_target: "/src/graft".to_string(),
            mode: MountMode::Rw,
        },
        // Second guest-visible bind into the sibling WITHOUT
        // a declared read-write carveout (denial-matrix
        // alternate route): declared RO, flipped RW for
        // Podman by the revision (outside every FULL grant),
        // so a read-execute readability rule — not a FULL
        // carveout, not a retained RO binding — denies writes.
        // The Landlock layer, not the VFS binding, carries
        // this denial.
        MountTriple {
            host_source: tree.path().join("sib").to_string_lossy().to_string(),
            container_target: "/alt/sib".to_string(),
            mode: MountMode::Ro,
        },
        // Denial-probe helper staging (read-only, disjoint
        // content — target/ never aliases the tree).
        MountTriple {
            host_source: probe_parent.to_string_lossy().to_string(),
            container_target: "/opt/probe".to_string(),
            mode: MountMode::Ro,
        },
        staged_triple,
    ];
    // Revision through the real FULL sets (same sets the
    // conductor revises with): `/src` flips RW for
    // materialization while `/src/proj/ro-data` (FULL target)
    // and `/extra/secret` (FULL-backed source) retain RO.
    let full_routes = full_grant_routes(&[fixture_hook()], &triples, tree.path(), &proj)
        .expect("full routes derive");
    let full_sources = cistella::mount::full_grant_sources(&triples, &proj);
    let revised = cistella::mount::revise_ro_for_confinement(&triples, &full_routes, &full_sources);
    let volumes = podman_volume_args(&revised, &session.container_home.clone(), None);
    let spec = CreateSpec {
        session,
        volumes,
        env: vec![],
        labels: vec![],
        landlock_hooked: true,
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
        _graft_src: graft_src,
        _staged: staged,
        client: Some(client),
        key,
        handle,
        container,
        triples,
        proj,
        graft_src: graft_dir,
        probe,
        guard,
    }
}

impl HookFixture {
    fn client(&self) -> &WireClient {
        self.client.as_ref().expect("client live")
    }

    fn teardown(&mut self) {
        // No-op-safe: explicit end-of-test teardown takes the
        // client; Drop re-entry finds None. Best-effort on every
        // path (wire errors ignored, teardown converges).
        let Some(client) = self.client.take() else {
            return;
        };
        let _ = client.terminate(&self.handle, Default::default(), &self.key);
        let _ = client.remove(&self.handle, &self.key);
        let _ = client.close();
    }
}

impl Drop for HookFixture {
    /// Close-on-unwind: a live assertion panic still terminates,
    /// removes, and closes through the client (then the name
    /// guard converges by name and staging drops) — no leaked
    /// guest worker, socket, unit, or staging dir. Mirrors the
    /// 2.3 fixture discipline.
    fn drop(&mut self) {
        self.teardown();
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

/// Denial matrix: every write-capable operation fails with
/// machine-readable EACCES under the ruleset, each with a
/// same-operation unconfined control on the same guest path
/// attributing the denial to Landlock (not a pre-existing
/// kernel wall), plus the alternate-bind route without a
/// carveout. The probe helper performs exact syscalls
/// (including `O_RDONLY|O_TRUNC`, the ABI-3 shape shell
/// redirection cannot express) and records `OK` or
/// `ERRNO=<n>` to a result file; any other errno fails.
/// ABI floor rides the per-harness ABI >= 3 gate (TRUNCATE
/// right); insufficient ABI fails pre-execute through the
/// probe's typed Unsupported, fast-pinned, unproducible on
/// supporting kernels.
#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn hook_denial_matrix_confines() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let Some(image) = fixture_image_opt() else {
        return;
    };
    let mut fixture = hook_fixture(&image);
    let tree = fixture.proj.parent().expect("tree").to_path_buf();
    // Both sibling routes translate through the validated
    // topology: the primary bind and the carveout-free
    // alternate bind.
    let sib_routes = cistella::mount::guest_routes_for_host(&fixture.triples, &tree.join("sib"));
    assert_eq!(
        sib_routes,
        vec!["/alt/sib".to_string(), "/src/sib".to_string()]
    );
    // Probe plumbing: the helper records one machine line
    // per invocation to a result file under the FULL
    // subtree (writable in both contexts); the harness
    // itself exits 0 having reported, so the RESULT carries
    // the verdict, never the exit code. Same binary, same
    // op, same path, same uid and mounts in both contexts:
    // companion exec proves the operation, hooked harness
    // proves the denial.
    let result_host = |tag: &str| fixture.proj.join(format!("probe-{tag}.out"));
    let result_guest = |tag: &str| format!("/src/proj/probe-{tag}.out");
    let probe_argv = |tag: &str, op: &[&str]| {
        let mut argv = vec![fixture.probe.clone(), result_guest(tag), op[0].to_string()];
        argv.extend(op[1..].iter().map(|arg| (*arg).to_string()));
        argv
    };
    let read_result = |tag: &str| {
        std::fs::read_to_string(result_host(tag))
            .unwrap_or_else(|_| panic!("probe {tag} must record a result"))
            .trim()
            .to_string()
    };
    // Unconfined control: the exact op must succeed outside
    // restrictions, recording `OK`.
    let pre_ok = |tag: &str, op: &[&str]| {
        let result = result_guest(tag);
        let mut exec_argv = vec![fixture.probe.as_str(), result.as_str(), op[0]];
        exec_argv.extend_from_slice(&op[1..]);
        let out = podman_exec(&fixture.container, &exec_argv);
        assert!(
            out.status.success(),
            "probe spawn must succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(read_result(tag), "OK", "unconfined {op:?} must succeed");
    };
    // Confined attempt: the helper must run (exit 0 having
    // reported) and record exactly EACCES — any other errno
    // (ENOENT, EROFS, tool failure) fails the case.
    let post_denied = |tag: &str, op: &[&str]| {
        let outcome = run_harness(&fixture, &probe_argv(tag, op));
        assert!(
            matches!(outcome, ExecutionOutcome::Exited(0)),
            "probe harness must report, got {outcome:?}"
        );
        assert_eq!(
            read_result(tag),
            "ERRNO=13",
            "confined {op:?} must fail EACCES"
        );
    };
    // WRITE open of a pre-existing file (no create or
    // truncate flags — denied exactly by WRITE_FILE):
    // unconfined open replaces the first byte (proves the
    // write landed), content is restored, confined open
    // records EACCES with content byte-exact.
    std::fs::write(tree.join("sib/w"), "0123456789").expect("w fixture content");
    pre_ok("write-pre", &["open-wronly", "/src/sib/w"]);
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/w")).expect("w readable"),
        "x123456789",
        "unconfined write must replace the first byte"
    );
    std::fs::write(tree.join("sib/w"), "0123456789").expect("restore w content");
    post_denied("write-post", &["open-wronly", "/src/sib/w"]);
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/w")).expect("w readable"),
        "0123456789",
        "denied write must leave content intact"
    );
    std::fs::remove_file(tree.join("sib/w")).expect("clean w marker");
    // CREATE of a fresh name with pre/post control.
    pre_ok("create-pre", &["create-excl", "/src/sib/new"]);
    assert!(
        tree.join("sib/new").exists(),
        "unconfined create must materialize"
    );
    std::fs::remove_file(tree.join("sib/new")).expect("clean created file");
    post_denied("create-post", &["create-excl", "/src/sib/new"]);
    assert!(
        !tree.join("sib/new").exists(),
        "denied create must not materialize"
    );
    // UNLINK: unconfined remove works (proves the op), then
    // the victim is restored for the confined attempt, which
    // must leave it byte-exact. Victim setup is host-side
    // fixture (creation itself is proved by create-excl).
    std::fs::write(tree.join("sib/victim"), "x").expect("victim fixture content");
    pre_ok("unlink-pre", &["unlink", "/src/sib/victim"]);
    assert!(
        !tree.join("sib/victim").exists(),
        "unconfined unlink must remove"
    );
    std::fs::write(tree.join("sib/victim"), "x").expect("restore victim content");
    post_denied("unlink-post", &["unlink", "/src/sib/victim"]);
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/victim")).expect("victim readable"),
        "x",
        "denied unlink must leave the file"
    );
    // RENAME: unconfined move works both directions (the
    // restore uses the same op), then the confined move
    // must leave source in place and destination absent.
    std::fs::write(tree.join("sib/orig"), "x").expect("orig fixture content");
    pre_ok("rename-pre", &["rename", "/src/sib/orig", "/src/sib/moved"]);
    assert!(
        !tree.join("sib/orig").exists() && tree.join("sib/moved").exists(),
        "unconfined rename must move"
    );
    pre_ok(
        "rename-restore",
        &["rename", "/src/sib/moved", "/src/sib/orig"],
    );
    post_denied(
        "rename-post",
        &["rename", "/src/sib/orig", "/src/sib/moved"],
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/orig")).expect("orig readable"),
        "x",
        "denied rename must leave the source"
    );
    assert!(
        !tree.join("sib/moved").exists(),
        "denied rename must not materialize the destination"
    );
    // TRUNCATE with exactly O_RDONLY|O_TRUNC (the ABI-3
    // shape shell redirection cannot express): unconfined
    // open empties the file (proves truncation happened),
    // content is restored, confined open records EACCES
    // with content byte-exact.
    std::fs::write(tree.join("sib/trunc"), "0123456789").expect("trunc fixture content");
    pre_ok("trunc-pre", &["open-ro-trunc", "/src/sib/trunc"]);
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/trunc")).expect("trunc readable"),
        "",
        "unconfined O_RDONLY|O_TRUNC must empty the file"
    );
    std::fs::write(tree.join("sib/trunc"), "0123456789").expect("restore trunc content");
    post_denied("trunc-post", &["open-ro-trunc", "/src/sib/trunc"]);
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/trunc")).expect("trunc readable"),
        "0123456789",
        "denied truncate must leave content intact"
    );
    // Alternate-bind route without a carveout: same
    // pre/post discipline through the second bind, with
    // the file present for both (pure open reports ENOENT
    // on missing paths, not EACCES).
    std::fs::write(tree.join("sib/ctl"), "0123456789").expect("alt fixture content");
    pre_ok("alt-pre", &["open-wronly", "/alt/sib/ctl"]);
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/ctl")).expect("ctl readable"),
        "x123456789",
        "unconfined alternate-route write must replace the first byte"
    );
    std::fs::write(tree.join("sib/ctl"), "0123456789").expect("restore ctl content");
    post_denied("alt-post", &["open-wronly", "/alt/sib/ctl"]);
    assert_eq!(
        std::fs::read_to_string(tree.join("sib/ctl")).expect("ctl readable"),
        "0123456789",
        "denied alternate-route write must leave content intact"
    );
    fixture.teardown();
}

/// Hooked launch attests applied and confines: admitted write
/// succeeds, unaliased sibling write fails, disjoint graft
/// admits, FULL-sourced RO alias denies, retained-RO alias
/// denies — all under attestation.
/// The sibling denial path carries no RW alias anywhere (no
/// graft of its source): any FULL alias would admit through
/// it at the dentry layer, so the fixture keeps them disjoint
/// and the product refuses aliased topologies pre-create.
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
    let outcome = run_harness(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo ok > /src/proj/marker".to_string(),
        ],
    );
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
    // Denied harness: unaliased sibling write fails (no RW
    // alias on its dentries anywhere — the graft source is
    // disjoint), attestation still applied.
    let outcome = run_harness(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo escape > /src/sib/escape".to_string(),
        ],
    );
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
    // Graft admission (tier-2 declared-RW graft proof): the
    // disjoint content grafted RW at `/src/graft` admits writes
    // by declared intent.
    let outcome = run_harness(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo graft > /src/graft/marker".to_string(),
        ],
    );
    assert!(
        matches!(outcome, ExecutionOutcome::Exited(0)),
        "graft harness must exit 0, got {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.graft_src.join("marker"))
            .expect("graft marker readable")
            .as_str(),
        "graft\n"
    );
    // The graft must not leak into the tree: disjoint dentries
    // mean no alias admits elsewhere.
    assert!(
        !fixture
            .proj
            .parent()
            .expect("tree")
            .join("sib/marker")
            .exists(),
        "graft marker must not appear in the tree"
    );
    // RO-source alias denial (reverse-direction retention
    // proof): the declared-RO directory inside the FULL
    // subtree, bound outside all FULL routes, denies writes —
    // the retained Podman read-only binding enforces what the
    // subtree FULL would otherwise admit through the alias.
    // Read first (mount materialized — an ENOENT false-pass
    // cannot satisfy a successful read), then write-denied.
    let outcome = run_harness(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "cat /extra/secret/seed".to_string(),
        ],
    );
    assert!(
        matches!(outcome, ExecutionOutcome::Exited(0)),
        "alias read must exit 0, got {outcome:?}"
    );
    let outcome = run_harness(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo escape > /extra/secret/escape".to_string(),
        ],
    );
    assert!(
        !matches!(outcome, ExecutionOutcome::Exited(0)),
        "alias harness must fail, got {outcome:?}"
    );
    assert!(
        !fixture.proj.join("secret/escape").exists(),
        "alias file must not exist"
    );
    // RO-under-RW alias denial (tier-2 retention proof): the
    // declared-RO directory under the FULL subtree denies
    // writes through the alias — the retained Podman read-only
    // binding enforces what Landlock union cannot subtract.
    // Read first (same ENOENT discipline as the secret run).
    let outcome = run_harness(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "cat /src/proj/ro-data/seed".to_string(),
        ],
    );
    assert!(
        matches!(outcome, ExecutionOutcome::Exited(0)),
        "alias read must exit 0, got {outcome:?}"
    );
    let outcome = run_harness(
        &fixture,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo escape > /src/proj/ro-data/escape".to_string(),
        ],
    );
    assert!(
        !matches!(outcome, ExecutionOutcome::Exited(0)),
        "alias harness must fail, got {outcome:?}"
    );
    assert!(
        !fixture.proj.join("ro-data/escape").exists(),
        "alias file must not exist"
    );
    fixture.teardown();
}

/// The fixture hook request: staged-wrapper singleton prefix
/// (same shape the extension answers with). Shared by the
/// revision derivation and the argv composition so the two run
/// on one hook.
fn fixture_hook() -> GuestHookRequest {
    GuestHookRequest {
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
    }
}

/// Composes the hooked launch argv through the real derivation
/// (system baseline, ancestor, subtree, session mounts): the live
/// test exercises the same argv the conductor builds.
fn hooked_argv(fixture: &HookFixture, harness: &[String]) -> Vec<String> {
    compose_hook_argv(
        &[fixture_hook()],
        &fixture.triples,
        fixture.proj.parent().expect("tree"),
        &fixture.proj,
        harness,
    )
    .expect("compose hooked argv")
}

/// Runs one harness through a hooked launch: diagnostics pipe,
/// execute, production attestation gate (ABI carries TRUNCATE,
/// clean transition), then await. Returns the harness outcome
/// for the caller to classify.
fn run_harness(fixture: &HookFixture, harness: &[String]) -> ExecutionOutcome {
    let argv = hooked_argv(fixture, harness);
    let (diag_read, diag_write) =
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).expect("diagnostics pipe");
    let execution = {
        use std::os::fd::AsFd;
        fixture
            .client()
            .execute_launch_hooked(
                &fixture.handle,
                &argv,
                Some("/src/proj"),
                StdioBinding::Inherit,
                &fixture.key,
                diag_write.as_fd(),
            )
            .expect("hooked launch")
    };
    drop(diag_write);
    // Production gate (not first-line-only): attestation plus
    // transitioned plus EOF through the real classifier.
    let (abi, detail) =
        cistella::framework::hooks::gate_hook_attestation(&diag_read, Duration::from_secs(30))
            .expect("production gate passes");
    assert!(abi >= 3, "attested ABI carries TRUNCATE");
    assert_eq!(detail, None);
    drop(diag_read);
    fixture
        .client()
        .await_result(&execution, &CancelFlag::default())
        .expect("await harness")
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
