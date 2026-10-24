//! Selector grammar: ambiguity and exclusivity.

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn selectors_ambiguous_and_exclusive() {
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // Per-run label values: the registry is shared fleet-wide, so static
    // values could match leftovers from an aborted run.
    let stamp = {
        use std::time::{SystemTime, UNIX_EPOCH};
        format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000
        )
    };
    let role_a = format!("role=a-{stamp}");
    let role_b = format!("role=b-{stamp}");

    let (mut child_a, id_a, mut guard_a) = spawn_conduct(
        &home,
        &worktree_str,
        &["--identity", "alice", "--label", &role_a],
    );
    let (mut child_b, id_b, mut guard_b) = spawn_conduct(
        &home,
        &worktree_str,
        &["--identity", "alice", "--label", &role_b],
    );
    wait_active(&id_a);
    wait_active(&id_b);

    // Same directory matches both: ambiguous typed refusal listing candidates.
    let out = run_cistella(
        &home,
        &["enter", "--directory", &worktree_str, "--", "true"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("ambiguous"), "ambiguous refusal: {err}");

    // Mixing selector forms is usage error.
    let out = run_cistella(
        &home,
        &["enter", &id_a, "--directory", &worktree_str, "--", "true"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("exactly one"), "exclusive refusal: {err}");

    // Unknown prefix names the registry without matching.
    let out = run_cistella(&home, &["enter", "zzzzzzzzzzzzzzzzz", "--", "true"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no session"));

    // Full-id selector enters exactly that session (true exits 0, no PTY needed).
    // Commit-event edge: enter refuses before the launch
    // attestation commits, and service-active fires first —
    // gate the enter on attestation appearance, not the
    // service proxy.
    let container_a = format!("cistella-{id_a}");
    let _ = wait_attestation(&id_a, &container_a);
    let out = run_cistella(&home, &["enter", &id_a, "--", "true"]);
    assert!(
        out.status.success(),
        "enter by id: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // survey filter shows both; label selector terminates exactly one.
    let out = run_cistella(&home, &["survey", "--directory", &worktree_str]);
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        txt.contains(&id_a) && txt.contains(&id_b),
        "survey shows both: {txt}"
    );
    let out = run_cistella(&home, &["terminate", "--label", &role_a]);
    assert!(
        out.status.success(),
        "terminate by label: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = reap_bounded(&mut child_a, CHILD_REAP_TIMEOUT)
        .expect("reap child_a post-terminate (missing reap is wedged-child evidence)");
    guard_a.id = None;

    let out = run_cistella(&home, &["survey", "--directory", &worktree_str]);
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !txt.contains(&id_a) && txt.contains(&id_b),
        "survey shows survivor: {txt}"
    );

    let out = run_cistella(&home, &["terminate", &id_b]);
    assert!(
        out.status.success(),
        "terminate {id_b}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = reap_bounded(&mut child_b, CHILD_REAP_TIMEOUT)
        .expect("reap child_b post-terminate (missing reap is wedged-child evidence)");
    guard_b.id = None;
}
