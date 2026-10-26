//! Operation-log pins: gc decisions and outcome vocabulary.
//!
//! Split from `operation_log.rs` at the file-size limit; shared
//! fixtures live there.

use super::operation_log::state_root;
use cistella::operation_log::{Budget, Injected, Operation, Outcome, op_id_valid, read_operation};
#[test]
fn classify_records_markers_for_unread_fields() {
    // The classify record distinguishes observed facts from
    // markers: uninspected units report not-queried, and the
    // full container id is unavailable (the classifier never
    // reads it), rather than invented.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    cistella::operation_log::record_gc_classify(
        &mut Some(op),
        &cistella::operation_log::Classify {
            name: "cistella-abc".to_string(),
            sid: "abc".to_string(),
            inspect_ok: true,
            inspect_err: String::new(),
            status: Some("exited".to_string()),
            exists: "present".to_string(),
            exists_err: None,
            unit_active: cistella::operation_log::Observed::NotQueried,
            unit_load: cistella::operation_log::Observed::NotQueried,
            decision: "reap-exited-container".to_string(),
        },
    );
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open2");
    drop(op);
    let read = read_operation(&dir).expect("read");
    let classify = read
        .records
        .iter()
        .find(|r| r.get("phase").and_then(|v| v.as_str()) == Some("classify"))
        .expect("classify record");
    assert_eq!(
        classify.get("unit_active").and_then(|v| v.as_str()),
        Some("not-queried")
    );
    assert_eq!(
        classify.get("container_id").and_then(|v| v.as_str()),
        Some("unavailable")
    );
    assert_eq!(
        classify.get("attempt").and_then(|v| v.as_str()),
        Some("not-queried")
    );
    assert_eq!(
        classify.get("reconciliation_key").and_then(|v| v.as_str()),
        Some("not-queried")
    );
}

#[test]
fn scoped_error_is_a_failure_for_the_pruner() {
    // The shared outcome vocabulary: an Err through scoped closes
    // with "failure", which the pruner files under the protected
    // failure reserve instead of mistaking it for a success. With
    // a zero success cap the success victim goes while the
    // failure survives on its reserve; without the shared
    // vocabulary both would be counted as successes and both
    // would go.
    let root = state_root();
    let log_root = root.path().join("logs");
    let ok = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let ok_dir = ok.dir().to_path_buf();
    ok.close("success");
    let failing = cistella::operation_log::scoped_at(&log_root, "gc", None, |_| {
        Err::<(), _>(cistella::error::CistellaError::Runtime("boom".to_string()))
    });
    assert!(failing.is_err(), "business error propagates");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 0,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "exactly the success victim goes");
    assert!(!ok_dir.exists(), "success victim evicted");
    let failed: Vec<_> = std::fs::read_dir(&log_root)
        .expect("list")
        .flatten()
        .filter(|e| {
            e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && op_id_valid(&e.file_name().to_string_lossy())
                && e.file_name().to_string_lossy() != "evicted"
        })
        .collect();
    assert_eq!(failed.len(), 1, "failure survives on its reserve");
    let read = read_operation(&failed[0].path()).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Completed(ref s) if s == "failure"),
        "failure outcome recorded, not success"
    );
}

#[test]
fn unknown_completed_outcome_counts_as_failure() {
    // The conservative default is load-bearing: a completed
    // outcome that is neither "success" nor "failure" must accrue
    // to the failure class, not silently pass as success. With a
    // zero success cap the success victim goes while the unknown
    // one survives on its reserve; reverting the rule to
    // "everything but failure is success" would evict both.
    let root = state_root();
    let log_root = root.path().join("logs");
    let ok = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    ok.close("success");
    let dir = log_root.join("abc123unknown");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut bytes = Vec::new();
    for (seq, kind, outcome) in [
        (0u64, "start", None),
        (1u64, "body", None),
        (2u64, "terminal", Some("weird")),
    ] {
        let mut payload = serde_json::json!({"seq": seq, "kind": kind});
        if let Some(outcome) = outcome {
            payload["outcome"] = serde_json::json!(outcome);
        }
        bytes.extend_from_slice(
            &cistella::operation_log::encode_frame(payload.to_string().as_bytes()).expect("encode"),
        );
    }
    std::fs::write(dir.join("records.log"), bytes).expect("write");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 0,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "only the success victim goes");
    assert!(
        dir.exists(),
        "unknown outcome survives in the failure class"
    );
}

#[test]
fn observed_distinguishes_queried_from_missing() {
    // The mapping the classifier uses: never-queried stays
    // not-queried, a missing key or an empty value is
    // unavailable (never the decision default), and a real value
    // passes through.
    use cistella::operation_log::Observed;
    assert_eq!(Observed::queried(None).as_str(), "unavailable");
    assert_eq!(Observed::queried(Some("")), Observed::Missing);
    assert_eq!(
        Observed::queried(Some("active")),
        Observed::Present("active".to_string())
    );
    assert_eq!(Observed::NotQueried.as_str(), "not-queried");
}

#[test]
fn missing_props_record_as_unavailable_not_defaults() {
    // A queried-but-missing unit property must read as
    // unavailable, never as the decision's historical defaults.
    // The observed values come from the real classifier mapping,
    // driven with an empty property map — not a hand-built
    // fixture — so this pins the producer-to-record wiring.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let classification =
        cistella::isolators::quadlet_gc::classify_candidate(None, Ok(false), Some(&empty), "");
    cistella::operation_log::record_gc_classify(
        &mut Some(op),
        &cistella::operation_log::Classify {
            name: "cistella-abc".to_string(),
            sid: String::new(),
            inspect_ok: false,
            inspect_err: String::new(),
            status: None,
            exists: classification.exists.to_string(),
            exists_err: classification.exists_err,
            unit_active: classification.unit_active,
            unit_load: classification.unit_load,
            decision: classification.decision.to_string(),
        },
    );
    let read = read_operation(&dir).expect("read");
    let classify = read
        .records
        .iter()
        .find(|r| r.get("phase").and_then(|v| v.as_str()) == Some("classify"))
        .expect("classify record");
    assert_eq!(
        classify.get("unit_active").and_then(|v| v.as_str()),
        Some("unavailable"),
        "missing is recorded, not the decision default"
    );
    assert_eq!(
        classify.get("unit_load").and_then(|v| v.as_str()),
        Some("unavailable"),
        "missing load is recorded, not the decision default"
    );
}

#[test]
fn ps_spawn_failure_records_abort_receipt() {
    // A podman spawn failure (nonexistent executable, no backend)
    // records the abort scan receipt with the actual invocation
    // error before the error propagates unchanged — the same
    // receipt a spawned-but-failed run gets.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    let mut slot = Some(op);
    let error = cistella::isolators::quadlet_gc::ps_candidate_names(
        &mut slot,
        "/nonexistent/cistella-test-no-podman",
    )
    .expect_err("spawn failure propagates");
    assert!(
        error.to_string().contains("podman ps"),
        "error still names the invocation: {error}"
    );
    let read = read_operation(&dir).expect("read");
    let scan = read
        .records
        .iter()
        .find(|r| r.get("phase").and_then(|v| v.as_str()) == Some("scan"))
        .expect("scan receipt recorded before propagation");
    assert_eq!(
        scan.get("decision").and_then(|v| v.as_str()),
        Some("aborted-ps-failed"),
        "spawn failure gets the abort receipt"
    );
    let detail = scan.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        detail.contains("No such file") || detail.contains("podman ps"),
        "receipt carries the actual invocation error, got: {detail}"
    );
}
