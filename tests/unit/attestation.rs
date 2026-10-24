//! Launch-attestation pins: naming, round-trip, refusal semantics.
//!
//! Storage-touching paths (publish/read/reap) run against a
//! temp state root; incarnation liveness (`query_incarnation`)
//! stays a live-tier concern (podman contact).

use cistella::attestation::{
    Attestation, CommitDecision, Currency, Incarnation, commit_decision, decide_currency,
    parse_identity,
};

fn sample() -> Attestation {
    Attestation {
        session_id: "abc123-def".to_string(),
        container_name: "cistella-abc123-def".to_string(),
        attempt_key: "key-1".to_string(),
        argv: vec!["sleep".to_string(), "infinity".to_string()],
        execution_handle: "exec-1".to_string(),
        incarnation: Some(Incarnation {
            container_id: "deadbeef".to_string(),
            started_at: "2026-10-20T00:00:00Z".to_string(),
        }),
    }
}

fn state_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmpdir")
}

#[test]
fn commit_decision_publishes_only_pre_bound_identity() {
    // Exact equality: the pre-bound identity publishes.
    let pre = Incarnation {
        container_id: "id-1".to_string(),
        started_at: "t-1".to_string(),
    };
    let same = pre.clone();
    assert_eq!(
        commit_decision(Some(&pre), Some(&same)),
        CommitDecision::Publish(pre.clone()),
        "equal pre/post publishes the bound identity"
    );
    // In-launch recreate (new ID): refuse, nothing published.
    let recreated = Incarnation {
        container_id: "id-2".to_string(),
        started_at: "t-1".to_string(),
    };
    assert!(
        matches!(
            commit_decision(Some(&pre), Some(&recreated)),
            CommitDecision::Refuse(_)
        ),
        "recreate inside the launch window refuses"
    );
    // In-launch restart (new StartedAt): refuse.
    let restarted = Incarnation {
        container_id: "id-1".to_string(),
        started_at: "t-2".to_string(),
    };
    assert!(
        matches!(
            commit_decision(Some(&pre), Some(&restarted)),
            CommitDecision::Refuse(_)
        ),
        "restart inside the launch window refuses"
    );
    // Unestablishable post-check: refuse (identity-unknown
    // never reads ready, never publishes).
    assert!(
        matches!(commit_decision(Some(&pre), None), CommitDecision::Refuse(_)),
        "unknown post-check refuses"
    );
    // Missing pre-capture: refuse even with a live post.
    assert!(
        matches!(
            commit_decision(None, Some(&same)),
            CommitDecision::Refuse(_)
        ),
        "missing pre-capture refuses"
    );
    assert!(
        matches!(commit_decision(None, None), CommitDecision::Refuse(_)),
        "missing pre-capture with unknown post refuses"
    );
}

#[test]
fn decide_currency_classifies_all_four_cases_without_backend() {
    // Pure classification pin: fabricated bound vs injected
    // observation, no podman. The event-wait helper asserts
    // these same outcomes live; these pins assert the
    // classification itself.
    let bound = Incarnation {
        container_id: "id-1".to_string(),
        started_at: "t-1".to_string(),
    };
    let same = bound.clone();
    assert_eq!(
        decide_currency(true, &bound, Some(&same)),
        Currency::Current,
        "equal live identity is current"
    );
    let recreated = Incarnation {
        container_id: "id-2".to_string(),
        started_at: "t-1".to_string(),
    };
    assert_eq!(
        decide_currency(true, &bound, Some(&recreated)),
        Currency::Stale,
        "new container ID is stale"
    );
    let restarted = Incarnation {
        container_id: "id-1".to_string(),
        started_at: "t-2".to_string(),
    };
    assert_eq!(
        decide_currency(true, &bound, Some(&restarted)),
        Currency::Stale,
        "new StartedAt is stale"
    );
    assert_eq!(
        decide_currency(true, &bound, None),
        Currency::Unknown,
        "unobservable live identity is unknown"
    );
    assert_eq!(
        decide_currency(false, &bound, Some(&same)),
        Currency::Mismatched,
        "container mismatch dominates liveness"
    );
    assert_eq!(
        decide_currency(false, &bound, None),
        Currency::Mismatched,
        "container mismatch dominates unknown too"
    );
}

#[test]
fn round_trip_preserves_all_fields() {
    let dir = state_root();
    let base = dir.path();
    let attestation = sample();
    cistella::attestation::publish(base, &attestation).expect("publish");
    let back = cistella::attestation::read(base, &attestation.session_id)
        .expect("read")
        .expect("present");
    assert_eq!(back, attestation);
}

#[test]
fn absent_reads_as_none_not_error() {
    let dir = state_root();
    let found = cistella::attestation::read(dir.path(), "never-published-1").expect("read");
    assert!(found.is_none(), "missing file is unattested, not an error");
}

#[test]
fn double_publish_refuses_write_once() {
    let dir = state_root();
    let base = dir.path();
    let attestation = sample();
    cistella::attestation::publish(base, &attestation).expect("first publish");
    let second = cistella::attestation::publish(base, &attestation);
    assert!(
        second.is_err(),
        "second publish must refuse (write-once), got: {second:?}"
    );
}

#[test]
fn corrupt_content_fails_loud() {
    let dir = state_root();
    let base = dir.path();
    let attestation = sample();
    cistella::attestation::publish(base, &attestation).expect("publish");
    // Corrupt the file through the same anchored dir the module
    // uses (proves the read path refuses, not the write path).
    let state_dir = cistella::lock::PrepareLease::state_dir(base);
    let anchored = cistella::guest_anchor::open_state_dir(&state_dir).expect("state dir opens");
    anchored
        .replace_file(
            &format!("cistella-attestation-{}", attestation.session_id),
            b"{not json",
        )
        .expect("corrupt");
    let result = cistella::attestation::read(base, &attestation.session_id);
    assert!(
        result.is_err(),
        "corrupt attestation must fail loud, got: {result:?}"
    );
}

#[test]
fn unknown_fields_refuse() {
    let bad = serde_json::json!({
        "session-id": "x",
        "container-name": "cistella-x",
        "attempt-key": "k",
        "argv": [],
        "execution-handle": "e",
        "incarnation": {"container-id": "i", "started-at": "s"},
        "future-field": true,
    });
    let parsed: Result<Attestation, _> = serde_json::from_value(bad);
    assert!(
        parsed.is_err(),
        "unknown attestation fields must refuse, got: {parsed:?}"
    );
}

#[test]
fn session_mismatch_refuses() {
    let dir = state_root();
    let base = dir.path();
    let attestation = sample();
    cistella::attestation::publish(base, &attestation).expect("publish");
    // Same file requested under a different session id: the
    // filename differs, so this reads absent — but a payload
    // whose embedded id disagrees with the request refuses.
    let mut tampered = attestation.clone();
    tampered.session_id = "other-session-9".to_string();
    let tampered_bytes = serde_json::to_vec(&tampered).expect("encodes");
    let state_dir = cistella::lock::PrepareLease::state_dir(base);
    let anchored = cistella::guest_anchor::open_state_dir(&state_dir).expect("state dir opens");
    anchored
        .replace_file(
            &format!("cistella-attestation-{}", sample().session_id),
            &tampered_bytes,
        )
        .expect("tamper");
    let result = cistella::attestation::read(base, &sample().session_id);
    assert!(
        result.is_err(),
        "id-mismatched payload must refuse, got: {result:?}"
    );
}

#[test]
fn incarnation_equality_is_exact() {
    // Primitive equality behind the commit decision: both
    // fields compare, neither a partial match.
    let a = Incarnation {
        container_id: "id-1".to_string(),
        started_at: "t-1".to_string(),
    };
    let same = a.clone();
    let new_id = Incarnation {
        container_id: "id-2".to_string(),
        started_at: "t-1".to_string(),
    };
    let restarted = Incarnation {
        container_id: "id-1".to_string(),
        started_at: "t-2".to_string(),
    };
    assert_eq!(a, same);
    assert_ne!(a, new_id, "recreate (new ID) must mismatch");
    assert_ne!(a, restarted, "restart (new StartedAt) must mismatch");
}

#[test]
fn reap_removes_without_failing() {
    let dir = state_root();
    let base = dir.path();
    let attestation = sample();
    cistella::attestation::publish(base, &attestation).expect("publish");
    cistella::attestation::reap(base, &attestation.session_id);
    let found = cistella::attestation::read(base, &attestation.session_id).expect("read");
    assert!(found.is_none(), "reaped attestation reads absent");
    // Reaping absence never fails.
    cistella::attestation::reap(base, &attestation.session_id);
    cistella::attestation::reap(base, "never-existed-2");
}

#[test]
fn foreign_commitment_round_trips_without_incarnation() {
    let dir = state_root();
    let base = dir.path();
    let mut attestation = sample();
    attestation.incarnation = None;
    cistella::attestation::publish(base, &attestation).expect("publish foreign");
    let back = cistella::attestation::read(base, &attestation.session_id)
        .expect("read")
        .expect("present");
    assert_eq!(back, attestation);
    assert!(
        back.incarnation.is_none(),
        "foreign commitment carries no incarnation"
    );
}

#[test]
fn empty_identity_fields_refuse_publish() {
    let dir = state_root();
    let mut attestation = sample();
    attestation
        .incarnation
        .as_mut()
        .expect("sample carries incarnation")
        .started_at
        .clear();
    let result = cistella::attestation::publish(dir.path(), &attestation);
    assert!(
        result.is_err(),
        "empty incarnation field must refuse publish, got: {result:?}"
    );
}

#[test]
fn orphan_sweep_reaps_only_dead_sessions() {
    let dir = state_root();
    let base = dir.path();
    let live = sample();
    let mut dead = sample();
    dead.session_id = "dead-session-7".to_string();
    dead.container_name = "cistella-dead-session-7".to_string();
    cistella::attestation::publish(base, &live).expect("publish live");
    cistella::attestation::publish(base, &dead).expect("publish dead");
    let live_set = std::collections::HashSet::from([live.session_id.clone()]);
    let reaped = cistella::attestation::reap_orphans(base, &live_set);
    assert_eq!(
        reaped.len(),
        1,
        "exactly the dead session reaps: {reaped:?}"
    );
    assert!(
        reaped[0].contains(&dead.session_id),
        "reaped name attributes the dead session: {reaped:?}"
    );
    let still = cistella::attestation::read(base, &live.session_id)
        .expect("read")
        .expect("live survives");
    assert_eq!(still, live);
}

#[test]
fn orphan_sweep_skips_unparseable_and_absent_dir() {
    let dir = state_root();
    let base = dir.path();
    // Absent state dir sweeps empty without creating noise.
    let empty = cistella::attestation::reap_orphans(base, &std::collections::HashSet::new());
    assert!(empty.is_empty());
    // Unparseable names skip (planted through the anchored dir
    // to prove the sweep — not the planter — skips).
    let state_dir = cistella::lock::PrepareLease::state_dir(base);
    let anchored = cistella::guest_anchor::open_state_dir(&state_dir).expect("state dir opens");
    {
        use std::io::Write as _;
        let mut f = anchored
            .create_file("cistella-attestation-!!!")
            .expect("plant");
        f.write_all(b"junk").expect("write");
    }
    {
        use std::io::Write as _;
        let mut f = anchored.create_file("unrelated-file").expect("plant");
        f.write_all(b"junk").expect("write");
    }
    let reaped = cistella::attestation::reap_orphans(base, &std::collections::HashSet::new());
    assert!(
        reaped.is_empty(),
        "unparseable names must skip, got: {reaped:?}"
    );
}

#[test]
fn parse_identity_keeps_spaceful_startedat_whole() {
    // Backend-version pin: podman 4.9 renders StartedAt as a
    // single token, 5.7 as a space-separated clock reading.
    // Only the first token boundary is structural; StartedAt
    // is the entire remainder, so same-day restarts stay
    // visible on spaceful renderings.
    let single =
        parse_identity("abc123 2026-10-24T14:51:00Z\n").expect("single-token rendering parses");
    assert_eq!(single.container_id, "abc123");
    assert_eq!(single.started_at, "2026-10-24T14:51:00Z");
    let spaced = parse_identity("abc123 2026-10-24 14:51:00 +0000 UTC\n")
        .expect("space-separated rendering parses whole");
    assert_eq!(spaced.container_id, "abc123");
    assert_eq!(spaced.started_at, "2026-10-24 14:51:00 +0000 UTC");
    assert!(
        parse_identity("abc123\n").is_err(),
        "id without StartedAt refuses"
    );
    assert!(parse_identity("   \n").is_err(), "blank rendering refuses");
    assert!(
        parse_identity("abc123   \n").is_err(),
        "id with blank StartedAt refuses"
    );
}
