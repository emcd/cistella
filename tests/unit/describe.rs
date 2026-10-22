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
    assert_eq!(render_state(&LifecycleState::Stopped), "stopped");
    assert_eq!(render_state(&LifecycleState::Absent), "absent");
}

#[test]
#[should_panic(expected = "snapshot_checked excludes ownerless executing")]
fn executing_has_no_rendering() {
    // Structural, not conventional: the arm panics instead of
    // mapping a token the verb must never report.
    let _ = render_state(&LifecycleState::Executing);
}

use cistella::attestation::{Attestation, Incarnation};

fn bound_attestation() -> Attestation {
    Attestation {
        session_id: "s-1".to_string(),
        container_name: "cistella-s-1".to_string(),
        attempt_key: "k".to_string(),
        argv: vec!["sleep".to_string()],
        execution_handle: "e".to_string(),
        incarnation: Some(Incarnation {
            container_id: "id".to_string(),
            started_at: "t".to_string(),
        }),
    }
}

fn ready_adm() -> cistella::describe::Admittability {
    cistella::describe::Admittability {
        running: true,
        init_pid: 42,
        unit_present: true,
    }
}

use cistella::describe::{EnterDecision, LiveIncarnation, decide_enter};

#[test]
fn enter_proceeds_when_all_edges_hold() {
    let attestation = bound_attestation();
    assert_eq!(
        decide_enter(
            Some(&attestation),
            "cistella-s-1",
            &LiveIncarnation::Current,
            &ready_adm(),
        ),
        EnterDecision::ProceedNative,
    );
}

#[test]
fn enter_refuses_before_commit() {
    let decision = decide_enter(
        None,
        "cistella-s-1",
        &LiveIncarnation::Unknown,
        &ready_adm(),
    );
    assert!(
        matches!(&decision, EnterDecision::Refuse(reason) if reason.contains("no launch attestation")),
        "missing attestation refuses typed: {decision:?}"
    );
}

#[test]
fn enter_refuses_container_mismatch() {
    let attestation = bound_attestation();
    let decision = decide_enter(
        Some(&attestation),
        "cistella-other",
        &LiveIncarnation::Current,
        &ready_adm(),
    );
    assert!(
        matches!(&decision, EnterDecision::Refuse(reason) if reason.contains("mismatch")),
        "container mismatch refuses typed: {decision:?}"
    );
}

#[test]
fn enter_refuses_stale_and_unknown_incarnation() {
    let attestation = bound_attestation();
    let stale = decide_enter(
        Some(&attestation),
        "cistella-s-1",
        &LiveIncarnation::Stale,
        &ready_adm(),
    );
    assert!(
        matches!(&stale, EnterDecision::Refuse(reason) if reason.contains("stale")),
        "stale incarnation refuses typed: {stale:?}"
    );
    let unknown = decide_enter(
        Some(&attestation),
        "cistella-s-1",
        &LiveIncarnation::Unknown,
        &ready_adm(),
    );
    assert!(
        matches!(&unknown, EnterDecision::Refuse(reason) if reason.contains("unestablishable")),
        "unknown incarnation refuses typed: {unknown:?}"
    );
}

#[test]
fn enter_refuses_unready_backend_each_condition_named() {
    let attestation = bound_attestation();
    let no_unit = cistella::describe::Admittability {
        unit_present: false,
        ..ready_adm()
    };
    assert!(
        matches!(
            decide_enter(Some(&attestation), "cistella-s-1", &LiveIncarnation::Current, &no_unit),
            EnterDecision::Refuse(reason) if reason.contains("unit file")
        ),
        "missing unit refuses naming it"
    );
    let stopped = cistella::describe::Admittability {
        running: false,
        ..ready_adm()
    };
    assert!(
        matches!(
            decide_enter(Some(&attestation), "cistella-s-1", &LiveIncarnation::Current, &stopped),
            EnterDecision::Refuse(reason) if reason.contains("not running")
        ),
        "stopped container refuses naming it"
    );
    let no_pid = cistella::describe::Admittability {
        init_pid: 0,
        ..ready_adm()
    };
    assert!(
        matches!(
            decide_enter(Some(&attestation), "cistella-s-1", &LiveIncarnation::Current, &no_pid),
            EnterDecision::Refuse(reason) if reason.contains("PID")
        ),
        "unestablished PID refuses naming it"
    );
}

#[test]
fn enter_routes_foreign_to_legacy_direct() {
    let mut attestation = bound_attestation();
    attestation.incarnation = None;
    assert_eq!(
        decide_enter(
            Some(&attestation),
            "cistella-s-1",
            &LiveIncarnation::Unknown,
            &cistella::describe::Admittability {
                running: false,
                init_pid: 0,
                unit_present: false,
            },
        ),
        EnterDecision::ProceedLegacyDirect,
        "foreign commitment bypasses the native gate entirely",
    );
}
