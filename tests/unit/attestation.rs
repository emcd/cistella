//! Launch-attestation pins: naming, round-trip, refusal semantics.
//!
//! Storage-touching paths (publish/read/reap) run against a
//! temp state root; incarnation liveness (`query_incarnation`)
//! stays a live-tier concern (podman contact).

use cistella::attestation::{Attestation, Incarnation, incarnation_matches};

fn sample() -> Attestation {
    Attestation {
        session_id: "abc123-def".to_string(),
        container_name: "cistella-abc123-def".to_string(),
        attempt_key: "key-1".to_string(),
        argv: vec!["sleep".to_string(), "infinity".to_string()],
        execution_handle: "exec-1".to_string(),
        incarnation: Incarnation {
            container_id: "deadbeef".to_string(),
            started_at: "2026-10-20T00:00:00Z".to_string(),
        },
    }
}

fn state_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmpdir")
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
    assert!(incarnation_matches(&a, &same));
    assert!(
        !incarnation_matches(&a, &new_id),
        "recreate (new ID) must mismatch"
    );
    assert!(
        !incarnation_matches(&a, &restarted),
        "restart (new StartedAt) must mismatch"
    );
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
fn empty_identity_fields_refuse_publish() {
    let dir = state_root();
    let mut attestation = sample();
    attestation.incarnation.started_at.clear();
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
