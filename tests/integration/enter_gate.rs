//! Enter readiness-gate tests: the commit-event edge and its
//! refusals (Change 2, issues/12).
//!
//! Split from `lifecycle.rs` at the file-size limit; the
//! terminate-while-attached conversion stays in place there,
//! gate refusal pins live here.

use std::process::Command;
use std::time::{Duration, Instant};

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_before_launch_commit() {
    // Negative control for the commit-event edge (issues/12):
    // a stalled conduct has installed its unit (record exists)
    // but committed no launch (no attestation), so `enter`
    // must refuse typed naming the missing attestation — never
    // exec into the unlaunched container. The 30s install/start
    // stall makes pre-commit deterministic; cleanup kills the
    // stalled child and terminates the leftover unit.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    // Learn the session id at INSTALL time, not from conduct
    // stdout: the id line prints only after the 30s stall,
    // ~1s before the attestation commits — reading it there
    // races publication (the gate can read present after the
    // test observed absent). The unit file lands at install
    // (pre-stall), so resolving it then leaves the whole
    // stall window for deterministic pre-commit asserts. Raw
    // spawn (no id-line wait) on the terminate_races precedent.
    let unit_dir = std::path::PathBuf::from(&home).join(".config/containers/systemd");
    let before: std::collections::HashSet<String> = std::fs::read_dir(&unit_dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut conduct = std::process::Command::new(bin())
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
        .env("CISTELLA_CONDUCT_START_DELAY_MS", "30000")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn stalled conduct");
    let (id, _unit_path) = wait_for_unit_file(&unit_dir, &before, &worktree_str);
    let mut guard = Guard {
        id: Some(id.clone()),
    };
    // Install completed (record resolvable) but start stalled:
    // bounded poll for the unit file, then prove pre-commit by
    // the attestation's absence (ids are minted unique, so no
    // sibling can have published for this session).
    let unit = unit_path(&home, &id);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !unit.exists() {
        if Instant::now() > deadline {
            panic!("unit file never installed for stalled conduct {id}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
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
    // Cleanup: kill the stalled child, terminate the leftover
    // unit, disarm the guard (it owns only this session).
    let _ = conduct.kill();
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "stalled unit terminates: {}",
        String::from_utf8_lossy(&term.stderr)
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
    assert_eq!(
        live_container_id(&container).as_deref(),
        Some(bound_id.as_str()),
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
