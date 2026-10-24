//! `describe state` no-record probe tests: the residue table
//! for full ids matching no registry record (Change 2).
//!
//! Split from `enter_gate.rs` by surface: these pin selection
//! and residue semantics, never the enter edge.

use super::helpers::*;
use super::parked::ParkedConduct;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn describe_state_absent_for_unknown_full_id() {
    // Typo truthfulness: a well-shaped but unknown full id with
    // no record, no container, and no attestation resolves
    // `absent` (native scope) with success — never an error,
    // never a fabricated lifecycle.
    if !systemd_gate() {
        return;
    }
    let home = home_dir();
    let id = cistella::session::mint_session_id();
    let out = run_cistella(&home, &["describe", "state", &id]);
    assert!(
        out.status.success(),
        "all-absent probe succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        shown.contains("state=absent") && shown.contains("commitment=unattested"),
        "all-absent reports absent+unattested, got: {shown}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn describe_state_residue_error_for_recordless_container() {
    // Backend fact dominates registry absence: a live container
    // with no unit file (hence no record) is residue — typed
    // error naming it, never an `absent` claim. Cleanup runs
    // before assertions so a failure never litters.
    if !systemd_gate() {
        return;
    }
    let home = home_dir();
    let id = cistella::session::mint_session_id();
    let container = format!("cistella-{id}");
    let run = run_bounded(
        {
            let mut cmd = std::process::Command::new("podman");
            cmd.args([
                "run",
                "-d",
                "--name",
                &container,
                "--label",
                &format!("cistella.id={id}"),
                "localhost/cistella/opencode:example",
                "sleep",
                "300",
            ]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman run stages the recordless container");
    assert!(
        run.status.success(),
        "podman run: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = run_cistella(&home, &["describe", "state", &id]);
    let rm = run_bounded(
        {
            let mut cmd = std::process::Command::new("podman");
            cmd.args(["rm", "-f", &container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman rm cleans up");
    assert!(
        rm.status.success(),
        "recordless container removed: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(
        !out.status.success(),
        "recordless live container must error, not report absent"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("residue") && stderr.contains(&container),
        "residue error names the container, got: {stderr}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn describe_state_absent_for_attestation_only_residue() {
    // Inert attestation residue (no record, no container)
    // resolves `absent` with the commitment bit set — dead
    // metadata, not a live session. The attestation is
    // published directly (test-as-owner fixture); reap follows.
    if !systemd_gate() {
        return;
    }
    let home = home_dir();
    let id = cistella::session::mint_session_id();
    let container = format!("cistella-{id}");
    let base = cistella::lock::runtime_base();
    cistella::attestation::publish(
        &base,
        &cistella::attestation::Attestation {
            session_id: id.clone(),
            container_name: container.clone(),
            attempt_key: "fixture-key".to_string(),
            argv: vec!["sleep".to_string()],
            execution_handle: "fixture-exec".to_string(),
            incarnation: Some(cistella::attestation::Incarnation {
                container_id: "fixture-container".to_string(),
                started_at: "fixture-start".to_string(),
            }),
        },
    )
    .expect("fixture attestation publishes");
    let out = run_cistella(&home, &["describe", "state", &id]);
    cistella::attestation::reap(&base, &id);
    assert!(
        out.status.success(),
        "inert residue probe succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        shown.contains("state=absent") && shown.contains("commitment=attested"),
        "inert residue reports absent+attested, got: {shown}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn describe_state_foreign_probe_without_backend_is_unsupported() {
    // Unincarnated (foreign) commitment with no local residue:
    // foreign absence cannot be established from local Podman
    // evidence, so the probe fails typed as unsupported rather
    // than inferring a global `absent`.
    if !systemd_gate() {
        return;
    }
    let home = home_dir();
    let id = cistella::session::mint_session_id();
    let container = format!("cistella-{id}");
    let base = cistella::lock::runtime_base();
    cistella::attestation::publish(
        &base,
        &cistella::attestation::Attestation {
            session_id: id.clone(),
            container_name: container,
            attempt_key: "fixture-key".to_string(),
            argv: vec!["sleep".to_string()],
            execution_handle: "fixture-exec".to_string(),
            incarnation: None,
        },
    )
    .expect("foreign fixture commitment publishes");
    let out = run_cistella(&home, &["describe", "state", &id]);
    cistella::attestation::reap(&base, &id);
    assert!(
        !out.status.success(),
        "foreign no-record probe must not report absent"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("unsupported"),
        "foreign probe fails as unsupported, got: {stderr}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn describe_state_contradictory_commitment_refuses_typed() {
    // Contradictory provenance pin: an unincarnated (foreign)
    // commitment attached to a record-holding session cannot
    // occur in production (foreign conducts create no unit
    // files), so describe fails closed loud instead of
    // rendering any shape. The fixture stages exactly that
    // contradiction over a stalled conduct's record.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    // Causal pre-initiate hold: the gate parks the conduct
    // after install, so the session provably never launches —
    // no container, no native attestation — and the write-once
    // fixture below cannot race a native publication. The id
    // is discovered at install, never from post-stall stdout.
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &profile, &gate);
    let id = parked.discover_id();
    let container = format!("cistella-{id}");
    let unit = unit_path(&home, &id);
    assert!(
        unit.exists(),
        "parked conduct installs its unit before the gate"
    );
    let base = cistella::lock::runtime_base();
    assert!(
        cistella::attestation::read(&base, &id)
            .expect("attestation reads")
            .is_none(),
        "no native attestation exists while parked pre-initiate"
    );
    cistella::attestation::publish(
        &base,
        &cistella::attestation::Attestation {
            session_id: id.clone(),
            container_name: container.clone(),
            attempt_key: "fixture-key".to_string(),
            argv: vec!["sleep".to_string()],
            execution_handle: "fixture-exec".to_string(),
            incarnation: None,
        },
    )
    .expect("fixture commitment publishes");
    let out = run_cistella(&home, &["describe", "state", &id]);
    assert!(
        !out.status.success(),
        "contradictory provenance must refuse, not render"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("contradictory provenance"),
        "refusal names the contradiction, got: {stderr}"
    );
    // Cleanup: abort converges the installed unit in-conduct
    // under the held creation lock, so no suite-parallel gc
    // window opens. The container never started (checked
    // query, not inference), and the test-created fixture
    // commitment is reaped through the product per-session
    // primitive — then proven gone — before disarming.
    let status = parked.abort();
    assert_eq!(
        status.code(),
        Some(143),
        "abort disposition is 128+SIGTERM, got: {status:?}"
    );
    assert!(!unit.exists(), "aborted startup converges the unit");
    assert!(
        container_absent(&container),
        "parked session never started a container: {container}"
    );
    cistella::attestation::reap(&base, &id);
    assert!(
        cistella::attestation::read(&base, &id)
            .expect("attestation reads")
            .is_none(),
        "fixture commitment reaped after abort"
    );
    parked.settle();
}
