//! Gc scope contract: parsing plus the visibility predicate.
//!
//! Backend-free pins for `CISTELLA_GC_SCOPE`: unset and blank
//! mean the legacy suite-wide scan, entries are trimmed with
//! empties dropped, and scoped runs observe only listed
//! sessions (empty ids never match). The full cross-test
//! isolation is demonstrated live; these tests pin the parsing
//! and predicate the live suite relies on.

use std::collections::{HashMap, HashSet};

use cistella::isolators::quadlet_gc::{
    GC_SCOPE_ENV, classify_candidate, parse_gc_scope, scope_allows,
    scope_from_env, scope_permits_reap,
};

fn set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn scope_env_name_is_stable() {
    // The child-process contract: helpers and gc agree on one
    // variable name, pinned here so a rename breaks loudly.
    assert_eq!(GC_SCOPE_ENV, "CISTELLA_GC_SCOPE");
}

#[test]
fn unset_scope_means_unscoped() {
    assert_eq!(parse_gc_scope(None), None);
    assert_eq!(scope_from_env(None), None);
}

#[test]
fn blank_explicit_scope_matches_nothing() {
    // Only absence authorizes the legacy sweep: an explicit
    // blank (or comma-only) setting matches nothing instead of
    // silently widening, so a misconfigured gc reaps nothing.
    for value in ["", "   ", ",,", " , , "] {
        assert_eq!(
            parse_gc_scope(Some(value)),
            Some(HashSet::new()),
            "blank scope must match nothing: {value:?}"
        );
        assert!(!scope_allows(&parse_gc_scope(Some(value)), "aaa"));
    }
    assert_eq!(scope_from_env(Some("".as_ref())), Some(HashSet::new()));
}

#[test]
fn unreadable_scope_matches_nothing() {
    // Non-Unicode variable bytes cannot name sessions; match
    // nothing rather than widen to the legacy sweep.
    use std::os::unix::ffi::OsStrExt;
    let raw = std::ffi::OsStr::from_bytes(b"\xff\xfe");
    assert_eq!(scope_from_env(Some(raw)), Some(HashSet::new()));
}

#[test]
fn scope_entries_trim_with_empties_dropped() {
    assert_eq!(
        parse_gc_scope(Some("  aaa, ,bbb ,,  ccc ")),
        Some(set(&["aaa", "bbb", "ccc"]))
    );
}

#[test]
fn single_entry_scope_parses() {
    assert_eq!(parse_gc_scope(Some("aaa")), Some(set(&["aaa"])));
}

#[test]
fn unscoped_sees_everything_including_empty() {
    assert!(scope_allows(&None, "aaa"));
    assert!(scope_allows(&None, ""));
}

#[test]
fn scoped_sees_only_listed_sessions() {
    let scope = parse_gc_scope(Some("aaa, bbb"));
    assert!(scope_allows(&scope, "aaa"));
    assert!(scope_allows(&scope, "bbb"));
    assert!(!scope_allows(&scope, "ccc"));
}

#[test]
fn empty_session_id_never_matches_scope() {
    // Unattributable candidates (no inspect id, no unit label)
    // stay untouched under scope: the run cannot prove they
    // are its own fixtures.
    let scope = parse_gc_scope(Some("aaa"));
    assert!(!scope_allows(&scope, ""));
    assert!(!scope_allows(&Some(HashSet::new()), "aaa"));
}

/// Inactive-unit property map: the orphan path with a readable
/// unit behind an absent container.
fn inactive_unit() -> HashMap<String, String> {
    let mut props = HashMap::new();
    props.insert("ActiveState".to_string(), "inactive".to_string());
    props.insert("LoadState".to_string(), "loaded".to_string());
    props
}

#[test]
fn scoped_reap_requires_resolved_member_identity() {
    let scope = parse_gc_scope(Some("aaa"));
    // In-scope resolved orphan reaps.
    let own =
        classify_candidate(None, Ok(false), Some(&inactive_unit()), "aaa");
    assert!(own.is_orphan_unit);
    assert!(scope_permits_reap(&scope, "aaa", &own));
    // Foreign resolved orphan never reaches mutation.
    let foreign =
        classify_candidate(None, Ok(false), Some(&inactive_unit()), "bbb");
    assert!(foreign.is_orphan_unit);
    assert!(!scope_permits_reap(&scope, "bbb", &foreign));
    // Exited container without a label: reap decision, empty
    // identity — stays put under scope despite the decision.
    let unlabeled = classify_candidate(Some("exited"), Ok(true), None, "");
    assert!(unlabeled.is_exited_container);
    assert!(!scope_permits_reap(&scope, "", &unlabeled));
    // Orphan unit with an unreadable id: same bar.
    let unreadable =
        classify_candidate(None, Ok(false), Some(&inactive_unit()), "");
    assert!(unreadable.is_orphan_unit);
    assert!(!scope_permits_reap(&scope, "", &unreadable));
    // Non-reap decisions never reach mutation either way.
    let live = classify_candidate(Some("running"), Ok(true), None, "aaa");
    assert!(!live.is_exited_container && !live.is_orphan_unit);
    assert!(!scope_permits_reap(&scope, "aaa", &live));
}

#[test]
fn unscoped_reap_keeps_legacy_behavior() {
    // Unscoped runs mutate exactly on the classification
    // decision, including unattributed identities.
    let exited = classify_candidate(Some("exited"), Ok(true), None, "");
    assert!(scope_permits_reap(&None, "", &exited));
    let orphan =
        classify_candidate(None, Ok(false), Some(&inactive_unit()), "");
    assert!(scope_permits_reap(&None, "", &orphan));
    let live = classify_candidate(Some("running"), Ok(true), None, "");
    assert!(!scope_permits_reap(&None, "", &live));
}

#[test]
fn empty_scope_set_reaps_nothing() {
    // Explicit-but-empty scope matches nothing, including on
    // the mutation path.
    let scope = Some(HashSet::new());
    let orphan =
        classify_candidate(None, Ok(false), Some(&inactive_unit()), "aaa");
    assert!(!scope_permits_reap(&scope, "aaa", &orphan));
}

#[test]
fn unresolved_present_withholds_dependent_sweeps() {
    use cistella::isolators::quadlet_gc::unresolved_live_possible;
    // Running container without a resolved identity: liveness
    // uncertain, sweeps must stand down.
    let unknown_live = classify_candidate(Some("running"), Ok(true), None, "");
    assert_eq!(unknown_live.decision, "unresolved-no-session");
    assert!(unresolved_live_possible(&unknown_live));
    // Exited containers are dead by status: sweeps unaffected.
    let exited = classify_candidate(Some("exited"), Ok(true), None, "");
    assert!(!unresolved_live_possible(&exited));
    // Absent candidates have no live backend behind them.
    let absent =
        classify_candidate(None, Ok(false), Some(&inactive_unit()), "aaa");
    assert!(!unresolved_live_possible(&absent));
    let absent_unknown = classify_candidate(None, Ok(false), None, "");
    assert!(!unresolved_live_possible(&absent_unknown));
    // Resolved live sessions are known, not uncertain.
    let live = classify_candidate(Some("running"), Ok(true), None, "aaa");
    assert!(!unresolved_live_possible(&live));
    // Fail-closed aborts precede sweeps regardless.
    let failed = classify_candidate(None, Ok(true), None, "");
    assert!(!unresolved_live_possible(&failed));
}

#[test]
fn scoped_sweep_retains_out_of_scope_attestations() {
    use cistella::attestation::{Attestation, Incarnation};
    fn sample(session: &str) -> Attestation {
        Attestation {
            session_id: session.to_string(),
            container_name: format!("cistella-{session}"),
            attempt_key: "key-1".to_string(),
            argv: vec!["sleep".to_string(), "infinity".to_string()],
            execution_handle: "exec-1".to_string(),
            incarnation: Some(Incarnation {
                container_id: "deadbeef".to_string(),
                started_at: "2026-10-20T00:00:00Z".to_string(),
            }),
        }
    }
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();
    let kept = sample("scopekept01");
    let out = sample("scopeout02");
    cistella::attestation::publish(base, &kept).expect("publish kept");
    cistella::attestation::publish(base, &out).expect("publish out");
    // Neither session is live; the scope names only the kept
    // one. The in-scope orphan reaps; the out-of-scope
    // attestation is retained despite missing from the live
    // set — the sweep never observes it.
    let scope = parse_gc_scope(Some("scopekept01"));
    let reaped = cistella::attestation::reap_orphans_scoped(
        base,
        &std::collections::HashSet::new(),
        scope.as_ref(),
    );
    assert_eq!(reaped.len(), 1, "exactly the in-scope orphan reaps");
    assert!(
        reaped[0].contains("scopekept01"),
        "reaped name attributes the in-scope session: {reaped:?}"
    );
    let retained = cistella::attestation::read(base, "scopeout02")
        .expect("read")
        .expect("out-of-scope attestation survives");
    assert_eq!(retained, out);
}
