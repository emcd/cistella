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
