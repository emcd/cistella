//! Enter readiness-gate tests: the commit-event edge and its
//! refusals (Change 2, issues/12).
//!
//! Split from `lifecycle.rs` at the file-size limit; the
//! terminate-while-attached conversion stays in place there,
//! gate refusal pins live here.

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
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &["sleep", "300"],
        &[("CISTELLA_CONDUCT_START_DELAY_MS", "30000")],
        None,
    );
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
