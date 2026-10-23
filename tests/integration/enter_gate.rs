//! Enter readiness-gate tests: the commit-event edge and its
//! refusals (Change 2 test surface).
//!
//! Split from `lifecycle.rs` at the file-size limit; the
//! terminate-while-attached conversion stays in place there,
//! gate refusal pins live here.

use std::process::Command;
use std::time::{Duration, Instant};

use super::helpers::*;

/// Kill-on-drop for a parked test child: armed at spawn,
/// before any fallible id discovery, so every exit path —
/// including a failed wait_for_unit_file or a panicked
/// assertion — kills the parked conduct instead of orphaning
/// it into a launch past the test. Reaping stays with the
/// normal path (reap_bounded); a Drop kill without reap leaves
/// only a zombie, never a running session.
struct KillOnDrop(Option<std::process::Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
        }
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_before_launch_commit() {
    // Negative control for the commit-event edge:
    // a gated conduct has installed its unit (record exists)
    // but cannot launch (no attestation ever commits), so
    // `enter` must refuse typed naming the missing
    // attestation — never exec into the unlaunched container.
    // The create→initiate boundary is held by an explicit
    // test-controlled handshake, not elapsed time: the conduct
    // parks opening the gate fifo (no product-side timeout),
    // the test asserts while launch is structurally unable to
    // proceed, then aborts via SIGTERM (the parked open has no
    // SA_RESTART, so the signal takes the abort path). The
    // KillOnDrop guard is armed at spawn — before any fallible
    // id discovery — so no path orphans the parked child into
    // a launch past the test.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    // The gate fifo is test-owned (not in the worktree): the
    // conduct blocks opening it for reading; no test open of
    // the write end ever happens here — the abort below is the
    // only release this test performs.
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate.fifo");
    nix::unistd::mkfifo(&gate, nix::sys::stat::Mode::from_bits(0o600).expect("mode"))
        .expect("mkfifo gate");
    // Install-completed unit files, pre-gate: the id is learned
    // from the installed unit, never from post-stall stdout
    // (which would race publication).
    let unit_dir = std::path::PathBuf::from(&home).join(".config/containers/systemd");
    let before: std::collections::HashSet<String> = std::fs::read_dir(&unit_dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    // Raw spawn (no id-line wait, no stderr lifetime drain)
    // on the terminate_races precedent: the stalled conduct
    // writes nothing before the test kills it, and the pipes
    // stay open so there is no SIGPIPE — but a future edit
    // that lets this child write freely must add a drain
    // thread or it will discover the hang, not the drain.
    let mut conduct = KillOnDrop(Some(
        std::process::Command::new(bin())
            .args([
                "conduct",
                "--profile",
                profile.as_str(),
                "--session-directory",
                &worktree_str,
                "--",
                "sleep",
                "300",
            ])
            .env("HOME", &home)
            .env("TERM", "xterm-ghostty")
            .env("CISTELLA_CONDUCT_START_GATE", &gate)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn gated conduct"),
    ));
    let (id, _unit_path) = wait_for_unit_file(&unit_dir, &before, &worktree_str);
    let mut guard = Guard {
        id: Some(id.clone()),
    };
    // Install is complete when wait_for_unit_file returns
    // (bounded: its timeout fails the test, never releases the
    // conduct): the record exists and the launch is parked at
    // the gate, so every assertion below runs while commit is
    // structurally impossible.
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
    // Cleanup is the abort half of the handshake: SIGTERM the
    // parked conduct (the gate has no product-side timeout, so
    // the abort is the only way it advances) and reap the
    // startup-abort disposition (128+SIGTERM — the same path
    // the existing signal-during-startup test pins). The abort
    // tears down the installed unit; the guard is disarmed
    // afterwards (it owns only this session).
    let pid = conduct.0.as_ref().expect("child alive").id();
    let mut kill = Command::new("kill");
    kill.args(["-TERM", &pid.to_string()]);
    run_bounded(kill, RUN_CISTELLA_TIMEOUT).expect("kill delivers");
    let status = reap_bounded(conduct.0.as_mut().expect("child alive"), CHILD_REAP_TIMEOUT)
        .expect("conduct reaped after abort");
    assert_eq!(
        status.code(),
        Some(143),
        "abort disposition is 128+SIGTERM, got: {status:?}"
    );
    guard.id = None;
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

/// Reads the live container StartedAt via the bound template.
/// None on transient failure — same contract as
/// `live_container_id`.
fn live_started_at(container: &str) -> Option<String> {
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["inspect", "--format", "{{.State.StartedAt}}", container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman inspect runs");
    if !out.status.success() {
        return None;
    }
    let started = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if started.is_empty() {
        return None;
    }
    Some(started)
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_stale_incarnation_after_restart() {
    // Restart changes StartedAt under a stable name: the
    // pre-restart commitment must not ready the new
    // incarnation. Ownerless setup (helper above) removes the
    // conduct-teardown race — the only writer is the restart.
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
    // Sanity: the edge passes while the incarnation is current
    // (ownerless direct exec of a no-op proves ProceedNative).
    let fresh = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        fresh.status.success(),
        "edge passes pre-restart: {}",
        String::from_utf8_lossy(&fresh.stderr)
    );
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
    // Bounded wait for the new incarnation (fail loud on a
    // restart that never lands — never assume the swap).
    // Transient inspect failures (remove/recreate window) read
    // as not-yet, not as readings: only a successful query
    // naming a different StartedAt breaks the loop.
    let bound_started = before
        .incarnation
        .as_ref()
        .expect("native commitment carries incarnation")
        .started_at
        .clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match live_started_at(&container) {
            Some(current) if current != bound_started => break,
            _ => {}
        }
        if Instant::now() > deadline {
            panic!("restart never landed a new StartedAt for {container}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        !out.status.success(),
        "post-restart enter must refuse, not exec"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stale"),
        "refusal names staleness, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
