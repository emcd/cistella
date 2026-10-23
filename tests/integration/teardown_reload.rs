//! Forced reload-failure exercise for `remove_unit_file`
//! (Change 2 ride-along, todos/50).
//!
//! The repair preserves the reload-on-unlink branch: when the
//! unit file unlinks but `daemon-reload` fails, teardown must
//! surface the reload error rather than converging. This test
//! forces that branch with a PATH-shimmed `systemctl` that
//! fails ONLY `daemon-reload` and delegates everything else to
//! the real binary — a live fault-injection seat, no product
//! seams widened.

use super::helpers::*;

use std::process::Command;

/// Resolves the real `systemctl` for shim delegation (fail-loud:
/// the shim must never shadow blindly).
fn real_systemctl() -> String {
    let out = std::process::Command::new("sh")
        .args(["-c", "command -v systemctl"])
        .output()
        .expect("command -v systemctl runs");
    assert!(
        out.status.success(),
        "live seat provides systemctl for delegation"
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Stages a `systemctl` shim that fails `daemon-reload` and
/// delegates every other invocation. Returns the shim dir
/// (kept alive by the caller for the test duration).
fn stage_reload_failing_shim(real: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("shim dir");
    std::fs::write(
        dir.path().join("systemctl"),
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--user\" ] && [ \"$2\" = \"daemon-reload\" ]; then\n  echo \"shimmed daemon-reload failure\" >&2\n  exit 1\nfi\nexec \"{real}\" \"$@\"\n"
        ),
    )
    .expect("write shim");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(
        dir.path().join("systemctl"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod shim");
    dir
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn teardown_surfaces_reload_failure_instead_of_converging() {
    // Fault-injection seat: a live session whose unit file
    // unlinks, then the reload fails. Teardown must report the
    // reload error (residue-class) — never converge, never
    // swallow. Recovery with the real systemctl converges.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &["sleep", "300"],
        &[],
        None,
    );
    let container = format!("cistella-{id}");
    wait_attestation(&id, &container);
    let unit = unit_path(&home, &id);
    assert!(unit.exists(), "unit installed pre-teardown");

    let real = real_systemctl();
    let shim = stage_reload_failing_shim(&real);
    let shim_path = format!(
        "{}:{}",
        shim.path().to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = spawn_cistella_cmd(&home, &["terminate", &id]);
    cmd.env("PATH", &shim_path);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("bounded terminate runs");
    assert!(
        !out.status.success(),
        "reload-failed teardown must not converge"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("daemon-reload failed"),
        "teardown surfaces the reload error, got: {stderr}"
    );
    // The unlink landed (caller-owned reload branch ran past
    // unlink) while the manager never reloaded: half-torn
    // state, honestly reported — not converged, not hidden.
    assert!(
        !unit.exists(),
        "unit file unlinked before the reload failure"
    );

    // Recovery is the operator path, not a second terminate:
    // the unit file is gone so no record resolves (record
    // resolution requires the file), while the container still
    // runs. Stop and remove the container directly, then let
    // `gc` reap the orphan attestation — the same sweep that
    // covers crashed conducts. A record-based terminate here
    // would (correctly) report `no session matches`; asserting
    // convergence through it would pin a resolution path the
    // product deliberately does not offer.
    let base = cistella::lock::runtime_base();
    for args in [&["stop", &container][..], &["rm", "-f", &container][..]] {
        let out = run_bounded(
            {
                let mut cmd = Command::new("podman");
                cmd.args(args);
                cmd
            },
            RUN_CISTELLA_TIMEOUT,
        )
        .expect("podman stop/rm runs");
        assert!(
            out.status.success(),
            "podman {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let collected = run_cistella(&home, &["gc"]);
    assert!(
        collected.status.success(),
        "gc converges: {}",
        String::from_utf8_lossy(&collected.stderr)
    );
    assert!(
        cistella::attestation::read(&base, &id)
            .expect("attestation reads")
            .is_none(),
        "gc reaps the orphan attestation"
    );
    let status = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("conduct reaped");
    eprintln!("conduct end: {status:?}");
    guard.id = None;
    assert!(
        cistella::runtime::residue_gone(&container),
        "no residue after recovery converge"
    );
}
