//! Orphan postmortem and gc reap: SIGKILLed conduct leaves a live-execution
//! unit for inspection, external stop orphans it, gc converges.
//!
//! Split from `lifecycle.rs` at the file-size limit.

use std::process::Command;
use std::time::Duration;

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn inspect_postmortem_and_gc_reap_orphan() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();

    let (mut conduct, id, mut guard) = spawn_conduct(&home, &worktree_str, &["--identity", "bob"]);
    wait_active(&id);
    let container = format!("cistella-{id}");
    // Harness-running (not just service-active): the guest binds
    // the execution on launch, and only a bound live execution is
    // preserved by the disconnect sweep — killing earlier races a
    // spec-mandated converge of the execution-less unit. Match the
    // harness ARGV (container PID1 sleeps too, so process counts
    // and bare "sleep" matches fire pre-harness).
    wait_harness(&container, "sleep 300");

    // SIGKILL conduct: no teardown runs, the orphan unit file remains.
    conduct.kill().expect("kill conduct");
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
    guard.id = None;
    std::thread::sleep(Duration::from_millis(500));
    assert!(unit_path(&home, &id).exists(), "orphan unit remains");

    // External stop simulating a crash: container gone (--rm), unit orphaned.
    // Hold the creation lock across stop-then-assert-then-inspect: a
    // sibling test's gc run in these windows would legitimately reap
    // our freshly-stopped unit (gc classifies absent-container +
    // unit-file + inactive-service as orphan), turning the presence
    // assertion — or the post-mortem inspect — into cross-test
    // timing. gc blocks on the lock; stop and the assertions never
    // take it. The freeze MUST drop before the final gc below: gc
    // acquires the same lock, and holding it across our own gc
    // would self-deadlock (separate process, same flock).
    let _creation_freeze = cistella::lock::LockGuard::acquire().expect("lock acquires");
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "stop", &format!("{container}.service")]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).unwrap();
    assert!(
        out.status.success(),
        "stop {container}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        unit_path(&home, &id).exists(),
        "unit file outlives external stop"
    );

    // inspect reads labels plus journald post-mortem from the orphan unit.
    let out = run_cistella(&home, &["inspect", &id]);
    assert!(
        out.status.success(),
        "inspect: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(txt.contains(&container), "inspect names session: {txt}");
    drop(_creation_freeze);

    // gc reaps the orphaned unit without manual pre-cleanup.
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!unit_path(&home, &id).exists(), "gc removes orphan unit");
}
