//! G4 pins: the gc's per-candidate classification is production
//! code driven directly. A missing or empty property map must read
//! as `Missing`, never as the decision defaults and never as
//! never-queried; real values pass through verbatim. These tests
//! drive [`classify_candidate`](cistella::isolators::quadlet_gc::classify_candidate)
//! itself — the mapping the gc records — not a hand-built fixture.

use std::collections::HashMap;

use cistella::isolators::quadlet_gc::{AbortKind, classify_candidate};
use cistella::operation_log::Observed;

fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn missing_props_classify_as_missing_not_defaults() {
    // Queried but absent: the unit was read and the keys are not
    // there. The record must say unavailable, never the
    // decision's historical defaults — and the decision still
    // orphans on unobserved input, preserved behavior.
    let empty: HashMap<String, String> = HashMap::new();
    let c = classify_candidate(None, Ok(false), Some(&empty), "");
    assert_eq!(c.unit_active, Observed::Missing);
    assert_eq!(c.unit_load, Observed::Missing);
    assert!(c.is_orphan_unit, "unobserved unit stays fail-closed");
    assert_eq!(c.decision, "reap-orphan-unit");
    assert_eq!(c.exists, "absent");
    assert_eq!(c.abort, None);
    assert!(!c.is_exited_container);
}

#[test]
fn empty_string_props_classify_as_missing() {
    // Present keys with empty values are equally unobserved. The
    // record says unavailable; the decision sees neither literal
    // default string, so it does not orphan on this input.
    let blank = props(&[("ActiveState", ""), ("LoadState", "")]);
    let c = classify_candidate(None, Ok(false), Some(&blank), "");
    assert_eq!(c.unit_active, Observed::Missing);
    assert_eq!(c.unit_load, Observed::Missing);
    assert!(!c.is_orphan_unit);
    assert_eq!(c.decision, "unresolved-no-session");
}

#[test]
fn observed_props_pass_through_and_decide_live() {
    // Real values ride through verbatim and decide live.
    let live = props(&[("ActiveState", "active"), ("LoadState", "loaded")]);
    let c = classify_candidate(None, Ok(false), Some(&live), "sess");
    assert_eq!(c.unit_active, Observed::Present("active".to_string()));
    assert_eq!(c.unit_load, Observed::Present("loaded".to_string()));
    assert!(!c.is_orphan_unit);
    assert_eq!(c.decision, "live");
    assert_eq!(c.abort, None);
}

#[test]
fn never_queried_paths_stay_not_queried() {
    // The present-container path never reads the unit.
    let c = classify_candidate(Some("running"), Ok(true), None, "sess");
    assert_eq!(c.unit_active, Observed::NotQueried);
    assert_eq!(c.unit_load, Observed::NotQueried);
    assert_eq!(c.decision, "live");
    assert_eq!(c.exists, "present");
    assert_eq!(c.abort, None);
    // Exited containers reap without any unit read.
    let c = classify_candidate(Some("exited"), Ok(true), None, "");
    assert!(c.is_exited_container);
    assert_eq!(c.decision, "reap-exited-container");
    assert_eq!(c.unit_active, Observed::NotQueried);
}

#[test]
fn exists_failure_carries_the_query_error_and_aborts() {
    // The receipt carries the actual query error, never the
    // unrelated inspect stderr, and the scan stops.
    let c = classify_candidate(None, Err("dial failed".to_string()), None, "");
    assert_eq!(c.exists, "query-failed");
    assert_eq!(c.exists_err.as_deref(), Some("dial failed"));
    assert_eq!(c.unit_active, Observed::NotQueried);
    assert_eq!(c.decision, "aborted-query-failed");
    assert_eq!(c.abort, Some(AbortKind::QueryFailed));
}

#[test]
fn present_but_uninspectable_aborts_fail_closed() {
    let c = classify_candidate(None, Ok(true), None, "");
    assert_eq!(c.exists, "present");
    assert_eq!(c.exists_err, None);
    assert_eq!(c.unit_active, Observed::NotQueried);
    assert_eq!(c.unit_load, Observed::NotQueried);
    assert_eq!(c.decision, "aborted-fail-closed");
    assert_eq!(c.abort, Some(AbortKind::FailClosed));
}
