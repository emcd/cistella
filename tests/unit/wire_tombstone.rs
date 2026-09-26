//! Wire-guest removal tombstones: repeatable teardown on evicted handles.
//!
//! Split from `wire_dispatch.rs` at the file-size limit. Successful
//! remove evicts the live binding into a tombstone keyed by handle:
//! identical convergent retries converge residue-free through
//! idempotent backend cleanup (never resurrection), divergent live
//! attempts — including same-handle create — refuse, and re-remove
//! clears planted residue. Runs fast with no Podman.

use cistella::framework::contract::ReconciliationKey;
use cistella::isolators::wire::{
    IsolatorGuest, OP_CREATE, OP_INITIATE, OP_INSPECT, OP_REMOVE, OP_STATE, OP_TERMINATE,
};

use super::wire_fake::{FakeBackend, create_payload, replay_spec};

fn remove_payload(fw: &str, key: &ReconciliationKey) -> serde_json::Value {
    serde_json::json!({"unit_handle": fw, "reconciliation_key": key.as_str()})
}

fn terminate_payload(fw: &str, key: &ReconciliationKey) -> serde_json::Value {
    serde_json::json!({
        "unit_handle": fw,
        "grace_ms": 1000u64,
        "reconciliation_key": key.as_str(),
    })
}

#[test]
fn remove_tombstones_repeatable_teardown() {
    // Successful remove evicts the live binding into a tombstone:
    // identical convergent retries report absent attestations
    // through exactly one idempotent backend cleanup each (no
    // resurrection, no live re-entry — creates and launches stay
    // flat while terminate/remove delegate once per retry).
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("tombstone01", &key, &spec))
        .expect("create must succeed");
    guest
        .dispatch(OP_REMOVE, &remove_payload("tombstone01", &key))
        .expect("remove must succeed");
    let stopped = guest
        .dispatch(OP_TERMINATE, &terminate_payload("tombstone01", &key))
        .expect("re-terminate reports absent");
    assert!(
        stopped.get("stopped_attestation").is_some(),
        "stopped attestation, got: {stopped}"
    );
    let removed = guest
        .dispatch(OP_REMOVE, &remove_payload("tombstone01", &key))
        .expect("re-remove reports absent");
    assert!(
        removed.get("removed_attestation").is_some(),
        "removed attestation, got: {removed}"
    );
    let state = guest
        .dispatch(OP_STATE, &serde_json::json!({"unit_handle": "tombstone01"}))
        .expect("state reports absent");
    assert_eq!(state, serde_json::json!({"lifecycle": "absent"}));
    let snapshot = guest
        .dispatch(
            OP_INSPECT,
            &serde_json::json!({"unit_handle": "tombstone01"}),
        )
        .expect("inspect reports absent");
    assert_eq!(
        snapshot
            .get("lifecycle")
            .and_then(|lifecycle| lifecycle.as_str()),
        Some("absent"),
        "absent snapshot, got: {snapshot}"
    );
    assert_eq!(
        backend.terminated.lock().unwrap().len(),
        1,
        "re-terminate delegates exactly once"
    );
    assert_eq!(
        backend.removed.lock().unwrap().len(),
        2,
        "re-remove delegates exactly once"
    );
    assert_eq!(
        *backend.creates.lock().unwrap(),
        1,
        "repeats create nothing"
    );
}

#[test]
fn tombstone_reremove_clears_planted_scratch() {
    // The delegation above is load-bearing, not incidental: scratch
    // planted AFTER the first remove must converge through a
    // tombstone re-remove (the exact absent-with-scratch shape the
    // live mirror proves end to end).
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("tombstone05", &key, &spec))
        .expect("create must succeed");
    guest
        .dispatch(OP_REMOVE, &remove_payload("tombstone05", &key))
        .expect("remove must succeed");
    backend
        .scratch
        .lock()
        .unwrap()
        .insert(spec.session.id.clone());
    guest
        .dispatch(OP_REMOVE, &remove_payload("tombstone05", &key))
        .expect("re-remove converges planted scratch");
    assert!(
        backend.scratch.lock().unwrap().is_empty(),
        "tombstone re-remove clears residue"
    );
}

#[test]
fn removed_handle_refuses_live_operations() {
    // Tombstones answer convergent reads, but live attempts never
    // re-enter the backend: initiate refuses removed (launch
    // follows the same resolve path once past the fd channel,
    // which unit tests never attach).
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("tombstone02", &key, &spec))
        .expect("create must succeed");
    guest
        .dispatch(OP_REMOVE, &remove_payload("tombstone02", &key))
        .expect("remove must succeed");
    let error = guest
        .dispatch(
            OP_INITIATE,
            &serde_json::json!({"unit_handle": "tombstone02", "reconciliation_key": key.as_str()}),
        )
        .expect_err("initiate on a removed handle must refuse");
    assert!(
        error.to_string().contains("removed"),
        "removed refusal, got: {error}"
    );
}

#[test]
fn create_over_tombstone_refuses() {
    // No re-create over a tombstone: the handle's lifecycle ended
    // at remove, and a same-handle replay must not resurrect the
    // unit or rerun its side effects — identical and divergent
    // attempts alike refuse. Fresh units arrive on fresh handles
    // (adopt-or-create keys those); the tombstone stays put, so
    // absence keeps reporting and backend creates stay flat.
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("tombstone03", &key, &spec))
        .expect("create must succeed");
    guest
        .dispatch(OP_REMOVE, &remove_payload("tombstone03", &key))
        .expect("remove must succeed");
    for (what, attempt_key) in [
        ("identical", key.clone()),
        ("divergent", ReconciliationKey::generate()),
    ] {
        let error = guest
            .dispatch(
                OP_CREATE,
                &create_payload("tombstone03", &attempt_key, &spec),
            )
            .expect_err(&format!("{what} re-create must refuse"));
        assert!(
            error.to_string().contains("removed"),
            "{what} refusal names removal, got: {error}"
        );
    }
    assert_eq!(
        *backend.creates.lock().unwrap(),
        1,
        "refused re-creates create nothing"
    );
    let state = guest
        .dispatch(OP_STATE, &serde_json::json!({"unit_handle": "tombstone03"}))
        .expect("tombstone still reports absent");
    assert_eq!(state, serde_json::json!({"lifecycle": "absent"}));
}

#[test]
fn tombstones_are_not_live_state() {
    // Tombstones are absence records: they never re-enter the
    // disconnect sweep, and a guest holding only tombstones is
    // quiescent (clean EOF).
    let backend = FakeBackend::default();
    let guest = IsolatorGuest::with_backend(backend.clone());
    let spec = replay_spec();
    let key = ReconciliationKey::generate();
    guest
        .dispatch(OP_CREATE, &create_payload("tombstone04", &key, &spec))
        .expect("create must succeed");
    assert!(guest.has_live_state(), "bound unit is live state");
    guest
        .dispatch(OP_REMOVE, &remove_payload("tombstone04", &key))
        .expect("remove must succeed");
    assert!(!guest.has_live_state(), "tombstone-only guest is quiescent");
    guest
        .converge_all(std::time::Duration::from_millis(100))
        .expect("converge skips tombstones cleanly");
    assert_eq!(
        backend.terminated.lock().unwrap().len(),
        0,
        "sweep terminates nothing tombstoned"
    );
    assert_eq!(
        backend.removed.lock().unwrap().len(),
        1,
        "only the original remove ran"
    );
    let state = guest
        .dispatch(OP_STATE, &serde_json::json!({"unit_handle": "tombstone04"}))
        .expect("tombstone survives the sweep");
    assert_eq!(state, serde_json::json!({"lifecycle": "absent"}));
}
