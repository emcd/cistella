//! Orphan postmortem and gc reap: SIGKILLed conduct leaves a live-execution
//! unit for inspection, external stop orphans it, gc converges.
//!
//! Split from `lifecycle.rs` at the file-size limit.

use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn inspect_postmortem_and_gc_reap_orphan() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
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

    // SIGKILL conduct: no teardown runs, unit file and scratch remain.
    conduct.kill().expect("kill conduct");
    let _ = conduct.wait();
    guard.id = None;
    std::thread::sleep(Duration::from_millis(500));
    assert!(unit_path(&home, &id).exists(), "orphan unit remains");

    // External stop simulating a crash: container gone (--rm), unit orphaned.
    // Hold the creation lock across stop-then-assert: a sibling
    // test's gc run in this window would legitimately reap our
    // freshly-stopped unit (gc classifies absent-container +
    // unit-file + inactive-service as orphan), turning the
    // presence assertion into cross-test timing. gc blocks on the
    // lock; stop and the assertions never take it.
    let _creation_freeze = cistella::lock::LockGuard::acquire().expect("lock acquires");
    let out = Command::new("systemctl")
        .args(["--user", "stop", &format!("{container}.service")])
        .output()
        .unwrap();
    assert!(out.status.success());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        unit_path(&home, &id).exists(),
        "unit file outlives external stop"
    );
    assert!(!scratch_gone(&id), "scratch outlives external stop");
    drop(_creation_freeze);

    // inspect reads labels plus journald post-mortem from the orphan unit.
    let out = run_cistella(&home, &["inspect", &id]);
    assert!(
        out.status.success(),
        "inspect: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(txt.contains(&container), "inspect names session: {txt}");

    // gc reaps the orphaned unit and scratch without manual pre-cleanup.
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!unit_path(&home, &id).exists(), "gc removes orphan unit");
    assert!(scratch_gone(&id), "gc removes orphan scratch");
}
