//! `describe state` pins: id shape, probe table, never-executing guard.
//!
//! Backend-contact paths (`probe_native`, `service_props`) stay
//! live-tier concerns; this file pins the pure decision table.

use cistella::describe::{
    NativeEvidence, ProbeOutcome, full_id_shape, probe_table, render_state, snapshot_checked,
};
use cistella::framework::isolator::UnitSnapshot;
use cistella::framework::lifecycle::LifecycleState;

fn evidence(container_present: bool, unit_present: bool) -> NativeEvidence {
    NativeEvidence {
        container_present,
        unit_present,
    }
}

#[test]
fn full_id_shape_matches_minted_only() {
    // 17 chars over the Crockford-lower alphabet.
    assert!(full_id_shape("0123456789abcdefg"));
    assert!(!full_id_shape("0123456789abcdef"), "short is a prefix");
    assert!(
        !full_id_shape("0123456789abcdefgx"),
        "long is not a full id"
    );
    assert!(!full_id_shape("0123456789ABCDEfg"), "uppercase never mints");
    assert!(!full_id_shape(""), "empty never matches");
    // Excluded Crockford letters (i, l, o, u) never mint.
    assert!(!full_id_shape("0123456789abcdefi"));
    assert!(!full_id_shape("0123456789abcdefl"));
    assert!(!full_id_shape("0123456789abcdefo"));
    assert!(!full_id_shape("0123456789abcdefu"));
}

#[test]
fn live_container_dominates_record_absence() {
    let outcome = probe_table("cistella-x", &evidence(true, false), false);
    assert_eq!(
        outcome,
        ProbeOutcome::Residue {
            container_name: "cistella-x".to_string()
        }
    );
    // Attestation residue beside a live container changes nothing.
    let outcome = probe_table("cistella-x", &evidence(true, true), true);
    assert!(
        matches!(outcome, ProbeOutcome::Residue { .. }),
        "live residue dominates even with attestation: {outcome:?}"
    );
}

#[test]
fn unit_file_without_record_is_residue() {
    // Records exist iff the unit file exists — a present file
    // with no record is a race or a stray, and backend fact
    // dominates either way (never an `absent` claim).
    let outcome = probe_table("cistella-x", &evidence(false, true), false);
    assert!(
        matches!(outcome, ProbeOutcome::Residue { .. }),
        "unit presence without record is residue: {outcome:?}"
    );
}

#[test]
fn inert_attestation_residue_resolves_absent() {
    let outcome = probe_table("cistella-x", &evidence(false, false), true);
    assert_eq!(outcome, ProbeOutcome::AbsentAttested);
}

#[test]
fn all_absent_resolves_absent() {
    let outcome = probe_table("cistella-x", &evidence(false, false), false);
    assert_eq!(outcome, ProbeOutcome::AbsentClean);
}

fn snapshot_with(lifecycle: LifecycleState) -> UnitSnapshot {
    UnitSnapshot {
        unit_identity: "cistella-x".to_string(),
        lifecycle,
        active_state: "active".to_string(),
        session_id: "x".to_string(),
        image: "img".to_string(),
    }
}

#[test]
fn non_executing_snapshots_pass_through() {
    for state in [
        LifecycleState::Created,
        LifecycleState::Initiated,
        LifecycleState::Stopped,
        LifecycleState::Absent,
    ] {
        let checked = snapshot_checked(&snapshot_with(state)).expect("passes");
        assert_eq!(checked, state);
    }
}

#[test]
fn ownerless_executing_refuses() {
    let result = snapshot_checked(&snapshot_with(LifecycleState::Executing));
    assert!(
        result.is_err(),
        "ownerless executing must refuse loud, got: {result:?}"
    );
}

#[test]
fn state_renders_verbatim_lowercase() {
    assert_eq!(render_state(&LifecycleState::Created), "created");
    assert_eq!(render_state(&LifecycleState::Initiated), "initiated");
    assert_eq!(render_state(&LifecycleState::Executing), "executing");
    assert_eq!(render_state(&LifecycleState::Stopped), "stopped");
    assert_eq!(render_state(&LifecycleState::Absent), "absent");
}
