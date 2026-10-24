//! Enter readiness-gate tests: the commit-event edge and its
//! refusals (Change 2 test surface).
//!
//! Split from `lifecycle.rs` at the file-size limit; the
//! terminate-while-attached conversion stays in place there,
//! gate refusal pins live here.

use std::process::Command;
use std::time::{Duration, Instant};

use super::helpers::*;
use super::parked::ParkedConduct;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_before_launch_commit() {
    // Negative control for the commit-event edge: a gated
    // conduct has installed its unit (record exists) but
    // cannot launch (no attestation ever commits), so `enter`
    // must refuse typed naming the missing attestation — never
    // exec into the unlaunched container. The create→initiate
    // boundary is held by an explicit test-controlled
    // handshake, not elapsed time: the conduct parks on the
    // gate file (no product-side timeout — absence of the file
    // never advances the session), so every assertion below
    // runs while commit is structurally unable to proceed.
    // ParkedConduct owns the child, pipes, and unit from spawn,
    // before any fallible id discovery; the test then aborts
    // via SIGTERM (128+SIGTERM, the startup-abort path).
    // Dropping that file here would be the release arm — this
    // test never creates it.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &profile, &gate);
    // Install is complete when discovery returns (bounded: its
    // timeout fails the test, never releases the conduct).
    let id = parked.discover_id();
    let base = cistella::lock::runtime_base();
    assert!(
        cistella::attestation::read(&base, &id)
            .expect("attestation read")
            .is_none(),
        "stalled conduct must not have committed"
    );
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        !out.status.success(),
        "pre-commit enter must refuse, not exec"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("no launch attestation"),
        "refusal names the missing attestation, got: {stderr}"
    );
    // Split contract: the same unattested session reports
    // successfully through `describe state` (observational,
    // exit 0) while `enter` refuses (nonzero above).
    let stated = run_cistella(&home, &["describe", "state", &id]);
    assert!(
        stated.status.success(),
        "describe state succeeds on unattested: {}",
        String::from_utf8_lossy(&stated.stderr)
    );
    let shown = String::from_utf8_lossy(&stated.stdout).to_string();
    assert!(
        shown.contains("commitment=unattested") && shown.contains("backend=unknown"),
        "describe reports unattested+unknown-backend, got: {shown}"
    );
    assert!(
        !shown.contains("state="),
        "no lifecycle snapshot without provenance, got: {shown}"
    );
    // Abort half of the handshake: SIGTERM the parked
    // conduct (the gate has no product-side timeout, so the
    // abort is the only way it advances) and reap the
    // startup-abort disposition (128+SIGTERM — the same path
    // the existing signal-during-startup test pins). The abort
    // tears down the installed unit; the id is disarmed
    // afterwards so Drop finds nothing left to converge.
    let status = parked.abort();
    assert_eq!(
        status.code(),
        Some(143),
        "abort disposition is 128+SIGTERM, got: {status:?}"
    );
    // The abort path converges the installed unit (terminate +
    // remove inside abort_startup): assert the teardown rather
    // than assuming it.
    assert!(
        !unit_path(&home, &id).exists(),
        "aborted startup leaves no unit file"
    );
    parked.settle();
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_parked_gate_aborts_on_pre_park_signal() {
    // Distinguishing ordering control for the check-then-park
    // question: the signal is delivered as early as causality
    // allows, so it may land before the first per-iteration
    // check or interrupt a parked poll — the outcome is
    // identical either way (abort, 143, unit converged),
    // because every path re-checks pending_signal and no path
    // advances on time. A blocking-open gate would park
    // forever in the pre-check-missed interleaving; this
    // polled gate cannot.
    //
    // Causality, not racing: the id is discovered first, which
    // requires the installed unit file — and signal handlers
    // are installed in main() long before create runs — so by
    // the time SIGTERM is sent, the disposition can only be
    // the abort path, never the pre-handler default. (Sending
    // before discovery could land before handler installation
    // and die by raw signal instead: a different, unasserted
    // path, and exactly the race this control refuses to run.)
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &profile, &gate);
    // Boundary arrival first: the installed unit proves create
    // ran, which proves the handlers are installed. Only then
    // signal — the abort that follows is ordered, not raced.
    let id = parked.discover_id();
    let status = parked.abort();
    assert_eq!(
        status.code(),
        Some(143),
        "pre-park signal aborts with 128+SIGTERM, got: {status:?}"
    );
    assert!(
        !unit_path(&home, &id).exists(),
        "aborted startup leaves no unit file"
    );
    parked.settle();
}

/// Spawns a conduct, waits for its launch commitment, then kills
/// the owner: the session stays fully resourced (unit, running
/// container, attestation) with nobody to tear it down, so
/// out-of-band container surgery cannot race conduct
/// convergence. Returns the session triple with a guard armed
/// for panic-path teardown.
fn ownerless_committed_session(home: &str, worktree_str: &str) -> (String, String, Guard) {
    let profile = fixture_profile("default.toml");
    let (mut conduct, id, guard) = spawn_conduct_full(
        home,
        &profile,
        worktree_str,
        &[],
        &["sleep", "300"],
        &[],
        None,
    );
    let container = format!("cistella-{id}");
    let attested = wait_attestation(&id, &container);
    assert_eq!(
        attested.container_name, container,
        "attestation binds this session's container"
    );
    // Ownerless from here: the harness outlives its driver, and
    // the guard owns teardown on every path below.
    let _ = conduct.kill();
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
    (id, container, guard)
}

/// Reads the live container ID via the same inspect template
/// the attestation binds. Returns None when the container is
/// transiently uninspectable (remove/recreate window) — the
/// caller keeps polling and fails loud only on the bound, never
/// mistaking a mid-swap absence for a reading.
fn live_container_id(container: &str) -> Option<String> {
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["inspect", "--format", "{{.Id}}", container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman inspect runs");
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() {
        return None;
    }
    Some(id)
}

/// Reads the live (Id, StartedAt) pair with one `podman
/// inspect`: one command, one observation, parsed exactly
/// like the commit binding (first two whitespace tokens;
/// absent or malformed reads as not-yet, never as absence).
fn live_identity(container: &str) -> Option<(String, String)> {
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args([
                "inspect",
                "--format",
                "{{.Id}} {{.State.StartedAt}}",
                container,
            ]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman inspect runs");
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(id), Some(started)) if !id.is_empty() && !started.is_empty() => {
            Some((id.to_string(), started.to_string()))
        }
        _ => None,
    }
}

/// Waits for a post-surgery incarnation to land as one
/// coherent observation: a single paired reading differing
/// from the bound. Transient uninspectability reads as
/// not-yet, never as absence; the bound fails loud. Landing
/// is the premise — the product gate renders the verdict.
fn wait_landed_incarnation(container: &str, bound_id: &str, bound_started: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match live_identity(container) {
            Some((id, started)) if id != bound_id || started != bound_started => return,
            _ => {}
        }
        if Instant::now() > deadline {
            panic!("surgery never landed a new incarnation for {container}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_stale_incarnation_after_restart() {
    // Live arm of the stale-incarnation split: after a restart
    // lands, entry into the replacement must be refused at
    // the readiness edge — never executed. This arm asserts
    // the refusal, not the word: both the stale and the
    // unestablishable refusals satisfy the safety property,
    // and the receipt reports which one fired. The stale
    // classification itself is pinned deterministically
    // beside `gate_enter` (no backend). Ownerless setup
    // (helper above) removes the conduct-teardown race — the
    // only writer is the restart.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (id, container, mut guard) = ownerless_committed_session(&home, &worktree_str);
    let base = cistella::lock::runtime_base();
    let before = cistella::attestation::read(&base, &id)
        .expect("attestation reads")
        .expect("committed");
    let bound = before
        .incarnation
        .as_ref()
        .expect("native commitment carries incarnation");
    let marker = "enter-probe";
    let probe = worktree.path().join(marker);
    // Positive current-incarnation entry evidence: the payload
    // executes and its side effect lands before the surgery.
    let fresh = run_cistella(
        &home,
        &["enter", &id, "--", "touch", &format!("/work/{marker}")],
    );
    assert!(
        fresh.status.success(),
        "edge passes pre-restart: {}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    assert!(probe.exists(), "pre-restart entry executes the payload");
    std::fs::remove_file(&probe).expect("clear the pre-restart marker");
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["restart", &container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman restart runs");
    assert!(
        out.status.success(),
        "podman restart: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Bounded wait for the replacement to land as one
    // coherent observation: a single paired reading differing
    // from the bound. Transient uninspectability reads as
    // not-yet, never as absence; the bound fails loud.
    // Landing is the premise — the product gate below renders
    // the verdict.
    wait_landed_incarnation(&container, &bound.container_id, &bound.started_at);
    // Readiness-edge negative control: the same payload must
    // not execute — its side effect stays absent — and the
    // refusal must come from the edge (stale or
    // unestablishable), never a selector or launch failure.
    let out = run_cistella(
        &home,
        &["enter", &id, "--", "touch", &format!("/work/{marker}")],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "post-restart enter must refuse, not exec"
    );
    assert!(!probe.exists(), "refused entry executes no payload");
    let verdict = if stderr.contains("stale") {
        "stale"
    } else if stderr.contains("unestablishable") {
        "unestablishable"
    } else {
        panic!("post-restart refusal must come from the readiness edge, got: {stderr}");
    };
    eprintln!("post-restart refusal: {verdict}");
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "restarted unit terminates: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    guard.id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_after_teardown_reports_selector_not_readiness() {
    // Deletion-ordering pin: teardown removes the unit file
    // before reaping the attestation, so a post-teardown enter
    // resolves to the Selector no-match — never a stale
    // readiness claim from residue, and never an `absent`
    // fabrication.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (id, container, mut guard) = ownerless_committed_session(&home, &worktree_str);
    let _ = container;
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "terminate converges: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    guard.id = None;
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(!out.status.success(), "post-teardown enter must fail");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("no session matches"),
        "record-gone dominates as Selector, got: {stderr}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_stale_incarnation_after_recreate() {
    // Out-of-band rm under Restart=on-failure: systemd recreates
    // the same name with a NEW container ID. The old commitment
    // must not ready the replacement. Same ownerless shape as
    // the restart test (ID change instead of StartedAt).
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (id, container, mut guard) = ownerless_committed_session(&home, &worktree_str);
    let base = cistella::lock::runtime_base();
    let before = cistella::attestation::read(&base, &id)
        .expect("attestation reads")
        .expect("committed");
    let bound_id = before
        .incarnation
        .as_ref()
        .expect("native commitment carries incarnation")
        .container_id
        .clone();
    // Pre-surgery identity, tolerantly: a transient inspect
    // failure here must retry, not fail — the loop below
    // already owns the fail-loud bound, this just establishes
    // the baseline it compares against.
    let deadline = Instant::now() + Duration::from_secs(60);
    let bound_live = loop {
        if let Some(current) = live_container_id(&container) {
            break current;
        }
        if Instant::now() > deadline {
            panic!("container never inspectable pre-surgery: {container}");
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(
        bound_live, bound_id,
        "attested ID matches the live container pre-surgery"
    );
    // Failure-inducing surgery: `podman kill` (SIGKILL) exits
    // the container as a failure, so the unit's
    // Restart=on-failure fires and systemd recreates the same
    // name with a new container ID. (`rm -f` exits clean and
    // never recreates — the wait below would be unreachable.)
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["kill", &container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman kill runs");
    assert!(
        out.status.success(),
        "podman kill: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Bounded wait for systemd's replacement (fail loud if the
    // service never recreates — never assume the swap).
    // Transient inspect failures (kill/recreate window) read as
    // not-yet: only a successful query naming a different ID
    // breaks the loop.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match live_container_id(&container) {
            Some(current) if current != bound_id => break,
            _ => {}
        }
        if Instant::now() > deadline {
            panic!("recreate never landed a new ID for {container}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        !out.status.success(),
        "post-recreate enter must refuse, not exec"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stale"),
        "refusal names staleness, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "recreated unit terminates: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    guard.id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_gate_release_proceeds_to_commit() {
    // Positive control for the release gate: creating the
    // release file lets the parked conduct proceed to initiate,
    // launch, and commit. Proves the gate does not wedge a
    // normal launch — the release branch is taken, not just
    // present. The abort path (refusal tests above) covers the
    // other arm.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &profile, &gate);
    let id = parked.discover_id();
    let container = format!("cistella-{id}");
    // Release: create the gate file; the next poll iteration
    // observes it and the conduct proceeds. The attestation
    // must then commit — proving the gate released rather
    // than wedged.
    std::fs::write(&gate, b"release").expect("release the gate");
    let attested = wait_attestation(&id, &container);
    assert_eq!(
        attested.container_name, container,
        "released conduct commits its launch"
    );
    // Normal teardown from here: terminate converges the
    // launched session, reap the conduct, then confirm the
    // unit is gone before disarming (Drop would converge it
    // identically, but the assertion belongs to the test).
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "released session terminates: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    let status = parked.reap_child();
    eprintln!("conduct end: {status:?}");
    assert!(
        !unit_path(&home, &id).exists(),
        "released session leaves no unit file"
    );
    parked.settle();
}
