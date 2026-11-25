//! Gc scope contract: parsing plus the visibility predicate.
//!
//! Backend-free pins for `CISTELLA_GC_SCOPE`: unset and blank
//! mean the legacy suite-wide scan, entries are trimmed with
//! empties dropped, and scoped runs observe only listed
//! sessions (empty ids never match). The full cross-test
//! isolation is demonstrated live; these tests pin the parsing
//! and predicate the live suite relies on.

use std::collections::HashSet;

use cistella::isolators::quadlet_gc::{
    GC_SCOPE_ENV, parse_gc_scope, scope_allows,
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
fn unset_and_blank_scope_means_unscoped() {
    assert_eq!(parse_gc_scope(None), None);
    assert_eq!(parse_gc_scope(Some("")), None);
    assert_eq!(parse_gc_scope(Some("   ")), None);
    assert_eq!(parse_gc_scope(Some(",,")), None);
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
