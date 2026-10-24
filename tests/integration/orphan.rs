//! Orphan postmortem and gc reap: SIGKILLed conduct leaves a live-execution
//! unit for inspection, external stop orphans it, gc converges.
//!
//! Split from `lifecycle.rs` at the file-size limit.

use std::process::Command;

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn inspect_postmortem_and_gc_reap_orphan() {
    if !systemd_gate() {
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
    // Real admission: the launch attestation binds the attempt
    // (unit, key, argv, execution handle) to this incarnation
    // after execute_launch returns Ok. Observed commit means the
    // execution is bound — the ARGV substring above is smoke
    // that cannot distinguish PID1's sleeper from the harness,
    // and killing on smoke alone races the execution-less
    // converge. Kill only past this commit.
    wait_attestation(&id, &container);

    // SIGKILL conduct: no teardown runs, the orphan unit file remains.
    // Hold the creation lock from BEFORE the kill across every
    // presence assertion below: SIGKILL is uncatchable so no
    // teardown path can run by product design, reap orders the
    // conduct's death, and the lock orders gc-exclusion (gc
    // blocks on it; a sibling gc in these windows would
    // legitimately reap our freshly-stopped unit and turn the
    // assertions into cross-test timing). With the only
    // deleter excluded, presence asserts immediately — no sleep
    // proxies "the kill took effect". The freeze MUST drop
    // before the final gc below: gc acquires the same lock, and
    // holding it across our own gc would self-deadlock
    // (separate process, same flock). Stop and the assertions
    // never take the lock themselves.
    let _creation_freeze = cistella::lock::LockGuard::acquire().expect("lock acquires");
    conduct.kill().expect("kill conduct");
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT)
        .expect("reap conduct post-teardown (missing reap is wedged-child evidence, not cleanup)");
    guard.id = None;
    assert!(unit_path(&home, &id).exists(), "orphan unit remains");

    // External stop simulating a crash: container gone (--rm), unit orphaned.
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "stop", &format!("{container}.service")]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).unwrap();
    assert!(
        out.status.success(),
        "stop {container}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Stop completion is ordered by the successful stop exit
    // above; gc-exclusion by the still-held freeze. Assert
    // immediately — no sleep proxies "the stop landed".
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
