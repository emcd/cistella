//! CLI-terminate vs attached-conduct convergence boundary.
//!
//! Live proof for `quadlet-remove-notfound-tolerant`: an external CLI
//! `terminate` (creation-lock held) races the attached conduct's own
//! unlocked teardown over the same unit file. Both actors must
//! converge — CLI reports `terminate`, conduct reports the harness
//! disposition — with no residue, regardless of which remover wins the
//! unlink. The exact loser-interleave (ENOENT) is pinned
//! deterministically in `tests/unit/quadlet_remove.rs`; this test
//! proves the two-actor convergence and final absence under real
//! systemd/Podman semantics. No new sleeps: readiness is state
//! predicates with the existing bounded backstops, and the creation
//! lock is never held across the concurrent window (holding it would
//! exclude the race under test).

use super::helpers::*;

/// Direct service state: `ActiveState` + `LoadState` for one unit.
/// The command outcome is checked: only explicit valid state evidence
/// counts — a failed query never proves absence.
fn service_state(id: &str) -> String {
    let out = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "ActiveState,LoadState",
            &format!("cistella-{id}.service"),
        ])
        .output()
        .expect("systemctl show");
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
    let out = std::process::Command::new("podman")
        .args(["container", "exists", container])
        .output()
        .expect("podman container exists");
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
        let _ = conduct.wait();
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

    // Conduct's disposition comes from the OBSERVED exit status: the
    // harness died to SIGTERM from the service stop, so conduct must
    // report 128+15 after converging its own teardown — the
    // documented shared-teardown outcome policy, with the retained
    // tap proving no teardown error hid behind the disposition.
    // Completion-attested tap: `child.wait` alone cannot guarantee
    // the drain consumed the final bytes.
    let conduct_status = conduct.wait().expect("conduct exits");
    let tap = taps.finalize(std::time::Duration::from_secs(10));
    assert!(
        tap.drain_clean,
        "conduct stderr fully drained before asserting on it: {}",
        tap.drain_note,
    );
    assert_eq!(
        conduct_status.code(),
        Some(143),
        "conduct reports observed harness SIGTERM disposition (128+15): status={conduct_status:?} conduct_stderr={} drain={}",
        tap.stderr,
        tap.drain_note,
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
    guard.id = None;
}
