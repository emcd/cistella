//! Unobservable-outcome pins: classify, wire encode, hello gate, renderer.
//!
//! Deterministic (no systemd/podman): exit statuses come from real
//! trivial subprocesses (`sh -c 'exit N'` / self-signal), facts are
//! fabricated — the decision table pins without a backend.

use std::process::Command;

use cistella::framework::isolator::{
    AbsenceObservation, ExecutionOutcome as Outcome, RawClientResult, UnobservableOutcome,
};
use cistella::isolators::corroboration::{CorroborationFacts, classify_completion};

fn exited(code: i32) -> std::process::ExitStatus {
    Command::new("sh")
        .args(["-c", &format!("exit {code}")])
        .status()
        .expect("spawn sh")
}

fn signaled(signum: i32) -> std::process::ExitStatus {
    Command::new("sh")
        .args(["-c", &format!("kill -{signum} $$")])
        .status()
        .expect("spawn sh")
}

fn absent_facts() -> CorroborationFacts {
    CorroborationFacts {
        container_exists_code: 1,
        service_active_state: "inactive".to_string(),
        service_load_state: "not-found".to_string(),
        unit_file_present: false,
    }
}

fn present_facts() -> CorroborationFacts {
    CorroborationFacts {
        container_exists_code: 0,
        service_active_state: "active".to_string(),
        service_load_state: "loaded".to_string(),
        unit_file_present: true,
    }
}

#[test]
fn conduct_signal_wins_before_status() {
    assert_eq!(
        classify_completion(Some(15), &exited(255), Some(&absent_facts())),
        Outcome::Signaled(15),
        "driver's own signal precedes any status reading"
    );
}

#[test]
fn zero_status_reports_directly() {
    assert_eq!(
        classify_completion(None, &exited(0), None),
        Outcome::Exited(0),
        "success needs no corroboration"
    );
}

#[test]
fn absent_container_with_failed_client_reports_unobservable() {
    assert_eq!(
        classify_completion(None, &exited(255), Some(&absent_facts())),
        Outcome::Unobservable(UnobservableOutcome {
            observation: AbsenceObservation {
                container_exists_code: 1,
                service_active_state: "inactive".to_string(),
                service_load_state: "not-found".to_string(),
                unit_file_present: false,
            },
            raw: RawClientResult::Exited(255),
        })
    );
    assert_eq!(
        classify_completion(None, &signaled(9), Some(&absent_facts())),
        Outcome::Unobservable(UnobservableOutcome {
            observation: AbsenceObservation {
                container_exists_code: 1,
                service_active_state: "inactive".to_string(),
                service_load_state: "not-found".to_string(),
                unit_file_present: false,
            },
            raw: RawClientResult::Signaled(9),
        })
    );
}

#[test]
fn present_container_keeps_legacy_mapping() {
    // Spoof control: removal-like failure with the container present
    // AND the service active is ordinary business, never an
    // interruption claim.
    assert_eq!(
        classify_completion(None, &exited(255), Some(&present_facts())),
        Outcome::Exited(255)
    );
    assert_eq!(
        classify_completion(None, &signaled(9), Some(&present_facts())),
        Outcome::Signaled(9),
        "signaled-present keeps legacy mapping by compatibility policy"
    );
    assert_eq!(
        classify_completion(None, &exited(137), Some(&present_facts())),
        Outcome::Exited(137),
        "ordinary nonzero exits unaffected"
    );
}

#[test]
fn deactivating_service_with_present_container_reports_unobservable() {
    // The Mode B shape: `container exists` is true for
    // stopped-but-present containers, so existence alone cannot
    // gate — a non-active service with a failed client is
    // observationally ambiguous (stop in flight) and reports
    // unobservable with the recorded states.
    let stopping = CorroborationFacts {
        container_exists_code: 0,
        service_active_state: "deactivating".to_string(),
        service_load_state: "loaded".to_string(),
        unit_file_present: true,
    };
    assert_eq!(
        classify_completion(None, &exited(137), Some(&stopping)),
        Outcome::Unobservable(UnobservableOutcome {
            observation: AbsenceObservation {
                container_exists_code: 0,
                service_active_state: "deactivating".to_string(),
                service_load_state: "loaded".to_string(),
                unit_file_present: true,
            },
            raw: RawClientResult::Exited(137),
        })
    );
}

#[test]
fn unavailable_service_state_keeps_legacy_mapping() {
    // Empty service strings (query unavailable, non-systemd seats):
    // absence unproven, never claimed.
    let unknown_service = CorroborationFacts {
        container_exists_code: 0,
        service_active_state: String::new(),
        service_load_state: String::new(),
        unit_file_present: true,
    };
    assert_eq!(
        classify_completion(None, &exited(255), Some(&unknown_service)),
        Outcome::Exited(255)
    );
}

#[test]
fn unobtainable_facts_keep_legacy_mapping() {
    // Query failure or unresolvable names: absence unproven, never
    // claimed.
    assert_eq!(
        classify_completion(None, &exited(255), None),
        Outcome::Exited(255)
    );
    assert_eq!(
        classify_completion(None, &signaled(9), None),
        Outcome::Signaled(9)
    );
}

#[test]
fn wire_encode_negotiation_gate() {
    use cistella::isolators::wire::encode_await_outcome;
    let unobservable = Outcome::Unobservable(UnobservableOutcome {
        observation: AbsenceObservation {
            container_exists_code: 1,
            service_active_state: "inactive".to_string(),
            service_load_state: "not-found".to_string(),
            unit_file_present: false,
        },
        raw: RawClientResult::Exited(255),
    });
    // Negotiated: typed variant with observation + raw.
    let typed = encode_await_outcome(&unobservable, true);
    assert_eq!(typed["unobservable"]["raw"]["exited"], 255);
    assert_eq!(
        typed["unobservable"]["observation"]["container_exists_code"],
        1
    );
    // Unnegotiated: downgrades to the legacy raw mapping exactly.
    assert_eq!(
        encode_await_outcome(&unobservable, false),
        serde_json::json!({"exit_status": 255})
    );
    let signaled_outcome = Outcome::Unobservable(UnobservableOutcome {
        observation: AbsenceObservation {
            container_exists_code: 1,
            service_active_state: "inactive".to_string(),
            service_load_state: "not-found".to_string(),
            unit_file_present: false,
        },
        raw: RawClientResult::Signaled(9),
    });
    assert_eq!(
        encode_await_outcome(&signaled_outcome, false),
        serde_json::json!({"signal": 9})
    );
    // Ordinary outcomes ignore the flag.
    assert_eq!(
        encode_await_outcome(&Outcome::Exited(0), true),
        serde_json::json!({"exit_status": 0})
    );
    assert_eq!(
        encode_await_outcome(&Outcome::Signaled(9), false),
        serde_json::json!({"signal": 9})
    );
}

#[test]
fn hello_capability_gate_pins() {
    use cistella::framework::protocol::{AWAIT_INTERRUPTED_V1, offered_capability};
    let offered = serde_json::json!({"capabilities": ["isolator", AWAIT_INTERRUPTED_V1]});
    assert!(offered_capability(&offered, AWAIT_INTERRUPTED_V1));
    let legacy = serde_json::json!({"capabilities": ["isolator"]});
    assert!(!offered_capability(&legacy, AWAIT_INTERRUPTED_V1));
    let missing = serde_json::json!({"version": 2});
    assert!(!offered_capability(&missing, AWAIT_INTERRUPTED_V1));
    let malformed = serde_json::json!({"capabilities": "isolator"});
    assert!(!offered_capability(&malformed, AWAIT_INTERRUPTED_V1));
}

#[test]
fn renderer_goldens_pin_shape_and_escaping() {
    use cistella::runtime::format_unobservable_report;
    let observation = AbsenceObservation {
        container_exists_code: 1,
        service_active_state: "inactive".to_string(),
        service_load_state: "not-found".to_string(),
        unit_file_present: false,
    };
    // Exact golden: one line, exited form, JSON-quoted states.
    assert_eq!(
        format_unobservable_report(&observation, &RawClientResult::Exited(255), true),
        "error: execution outcome unobservable (container absent at query, service \"inactive\"/\"not-found\" unit absent): harness outcome unobservable; raw client exited(255); cleanup=converged",
    );
    // Signaled form + unproven cleanup.
    assert_eq!(
        format_unobservable_report(&observation, &RawClientResult::Signaled(9), false),
        "error: execution outcome unobservable (container absent at query, service \"inactive\"/\"not-found\" unit absent): harness outcome unobservable; raw client signaled(9); cleanup=unproven",
    );
    // Terminal-control/newline injection in observed strings is
    // JSON-escaped, never emitted raw.
    let hostile = AbsenceObservation {
        container_exists_code: 1,
        service_active_state: "active\nerror: forged".to_string(),
        service_load_state: "loaded\x1b[2J".to_string(),
        unit_file_present: true,
    };
    let rendered = format_unobservable_report(&hostile, &RawClientResult::Exited(1), true);
    assert!(
        !rendered.contains('\n') && !rendered.contains('\x1b'),
        "no raw control bytes in rendering: {rendered:?}"
    );
    assert!(
        rendered.contains("unit present"),
        "unit presence renders verbatim: {rendered:?}"
    );
}

#[test]
fn renderer_reports_present_container_truthfully() {
    // The Mode B shape: container present (exit 0) with a
    // deactivating service must NOT print a fixed 'absent' claim —
    // the container fact renders from the recorded code.
    use cistella::runtime::format_unobservable_report;
    let observation = AbsenceObservation {
        container_exists_code: 0,
        service_active_state: "deactivating".to_string(),
        service_load_state: "loaded".to_string(),
        unit_file_present: true,
    };
    assert_eq!(
        format_unobservable_report(&observation, &RawClientResult::Exited(137), true),
        "error: execution outcome unobservable (container present at query, service \"deactivating\"/\"loaded\" unit present): harness outcome unobservable; raw client exited(137); cleanup=converged",
    );
}

#[test]
fn absence_proof_converged_rule_pins() {
    use cistella::isolators::corroboration::AbsenceProof;
    // Converged: strictly absent container + terminal service state
    // (LoadState not-found here) + absent unit file.
    assert!(AbsenceProof::new(1, "inactive".to_string(), "not-found".to_string(), true).converged);
    // Converged via the other terminal state (inactive ActiveState).
    assert!(AbsenceProof::new(1, "inactive".to_string(), "loaded".to_string(), true).converged);
    // Stopped-but-present container never converges (residue).
    assert!(!AbsenceProof::new(0, "inactive".to_string(), "loaded".to_string(), true).converged);
    // Active service never converges, even with absent container.
    assert!(!AbsenceProof::new(1, "active".to_string(), "loaded".to_string(), true).converged);
    // Non-terminal service states never converge: activating,
    // deactivating, failed, reloading.
    for state in [
        "activating",
        "deactivating",
        "failed",
        "reloading",
        "maintenance",
    ] {
        assert!(
            !AbsenceProof::new(1, state.to_string(), "loaded".to_string(), true).converged,
            "non-terminal service state must not converge: {state}"
        );
    }
    // Present unit file never converges.
    assert!(
        !AbsenceProof::new(1, "inactive".to_string(), "not-found".to_string(), false).converged
    );
    // Empty service strings (manager failure / query unavailable)
    // never converge: only explicit terminal states attest.
    assert!(!AbsenceProof::new(1, String::new(), String::new(), true).converged);
    // Non-0/1 container codes never converge.
    assert!(!AbsenceProof::new(125, "inactive".to_string(), "loaded".to_string(), true).converged);
}
