//! CLI-terminate vs attached-conduct convergence boundary.
//!
//! Live proof for outcome-unobservable reporting: an external CLI
//! `terminate` (creation-lock held) races the attached conduct's own
//! unlocked teardown over the same unit file. Conduct MUST NOT report
//! a harness disposition for the race it lost observability of (the
//! withdrawn fixed-143 premise); it reports the typed observation
//! with driver-report exit 1, with cleanup proven converged.
//! Cleanup convergence itself (unit/service/container absence) is
//! asserted as facts before cleanup does any work. The exact
//! loser-interleave (ENOENT) stays pinned deterministically in
//! `tests/unit/quadlet_remove.rs`; raw dispositions stay pinned per
//! interleave in `tests/unit/unobservable.rs`. No new sleeps:
//! readiness is state predicates with the existing bounded
//! backstops, and the creation lock is never held across the
//! concurrent window (holding it would exclude the race under test).

use super::helpers::*;

/// Direct service state: `ActiveState` + `LoadState` for one unit.
/// The command outcome is checked: only explicit valid state evidence
/// counts — a failed query never proves absence.
fn service_state(id: &str) -> String {
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args([
        "--user",
        "show",
        "-p",
        "ActiveState,LoadState",
        &format!("cistella-{id}.service"),
    ]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("systemctl show");
    assert!(
        out.status.success(),
        "systemctl show must succeed to observe service state: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// True only when the container is documented-absent: `podman
/// container exists` exits 1 for absent, 125 for command/storage
/// errors — 125 (or any other unexpected outcome) fails observation
/// instead of proving cleanup.
fn container_absent(container: &str) -> bool {
    let mut cmd = std::process::Command::new("podman");
    cmd.args(["container", "exists", container]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("podman container exists");
    match out.status.code() {
        Some(1) => true,
        Some(0) => false,
        other => panic!(
            "podman container exists returned {other:?} (want 0/present or 1/absent): stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ),
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn cli_terminate_converges_attached_conduct() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");

    // Attached conduct with conduct's own stderr retained (bounded
    // tap, not the sink): the teardown-error evidence this boundary
    // must produce lives in conduct's stderr, and an `inspect`
    // post-mortem cannot recover it.
    let (mut conduct, id, mut guard, mut taps) = spawn_conduct_tapped(
        &home,
        &profile,
        &worktree_str,
        &["--identity", "alice"],
        &["sleep", "300"],
        &[],
    );
    // Readiness gates "session fully up" before the CLI actor enters,
    // so any overlap below is teardown overlap. `wait_active` gates
    // service-active; `wait_harness` gates a running harness. Neither
    // proves guest execution-table registration — no host CLI surface
    // reads that table (`survey`/`inspect` show systemd state and
    // registry labels only; `enter` runs a NEW direct exec with no
    // happens-before edge to the launch binding, so it is deliberately
    // NOT used as acknowledgement here). A guest-state query surface
    // is 0.4 product work (no-wait-and-pray principle); the overlap
    // under test does not need it, because neither actor's removal
    // path consults the execution table — both act on the unit once
    // past startup. Both predicates are state-based with bounded
    // backstops, never durations-as-oracles.
    wait_active(&id);
    let container = format!("cistella-{id}");
    wait_harness(&container, "sleep 300");

    // The concurrent actor: external CLI terminate while conduct is
    // attached awaiting the harness. CLI stops the service (killing
    // the harness), conduct wakes and tears down the same unit —
    // one of them loses the unlink to NotFound and must converge.
    // On CLI failure the conduct actor is still joined and its
    // retained tap rendered: the panic carries BOTH actors' evidence,
    // never CLI stderr alone.
    let out = run_cistella(&home, &["terminate", &id]);
    if !out.status.success() {
        // Original CLI evidence emits BEFORE joining conduct: a
        // slow/wedged conduct wait must never suppress the immediate
        // diagnostics.
        eprintln!(
            "cli terminate failed: status={} stdout={} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
        let tap = taps.finalize(std::time::Duration::from_secs(10));
        panic!(
            "cli terminate converges: status={} stdout={} stderr={} conduct_stderr={} drain={}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            tap.stderr,
            tap.drain_note,
        );
    }

    // Conduct's report comes from the OBSERVED outcome set, never
    // a guessed harness disposition (the fixed-143 premise is
    // withdrawn): the container vanished mid-await, so conduct
    // reports outcome-unobservable with driver-report exit 1 after
    // converging its own teardown. The completion-attested tap
    // proves no teardown error hid behind the report, and the
    // report names the observation with the raw client result.
    // Completion-attested tap: `child.wait` alone cannot guarantee
    // the drain consumed the final bytes.
    let conduct_status = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("conduct exits");
    let tap = taps.finalize(std::time::Duration::from_secs(10));
    assert!(
        tap.drain_clean,
        "conduct stderr fully drained before asserting on it: {}",
        tap.drain_note,
    );
    assert_eq!(
        conduct_status.code(),
        Some(1),
        "conduct reports the typed observation as a driver-level report: status={conduct_status:?} conduct_stderr={} drain={}",
        tap.stderr,
        tap.drain_note,
    );
    assert!(
        tap.stderr.contains("execution outcome unobservable"),
        "typed observation report names itself: {}",
        tap.stderr,
    );
    assert!(
        tap.stderr.contains("cleanup=converged"),
        "cleanup claim proven before printing: {}",
        tap.stderr,
    );
    assert!(
        !tap.stderr.contains("error: teardown"),
        "conduct teardown converged without error: {}",
        tap.stderr,
    );

    // Final absence is asserted BEFORE cleanup does any work: the
    // guard still holds the id, so every reap here is the actors'
    // doing, not the fixture's. Three direct facts (not survey
    // inference): the unit file is gone, systemd reports the service
    // inactive/not-found, and the container is absent.
    assert!(
        !unit_path(&home, &id).exists(),
        "unit reaped by the racing removers, not cleanup"
    );
    let state = service_state(&id);
    assert!(
        state.contains("LoadState=not-found") || state.contains("ActiveState=inactive"),
        "service inactive/not-found after converged teardown: {state}"
    );
    assert!(
        container_absent(&container),
        "container absent after converged teardown: {container}"
    );
    // Per-run mode proof: passing runs print the typed observation
    // line (with the raw client result) so receipts distinguish
    // Mode A (exec-removed) from Mode B (stop-path) without
    // re-running. Stable single line for log aggregation.
    eprintln!("boundary converged: {}", tap.stderr.trim());
    guard.id = None;
}
