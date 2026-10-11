//! Durable prepare leases: create/update/remove lifecycle, all
//! against explicit tempdir bases (no process env — parallel-safe
//! and hermetic).

use cistella::lock::PrepareLease;

#[test]
fn lease_create_record_remove_lifecycle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease = PrepareLease::create(dir.path(), "test-session-1").expect("lease creates");
    let path = dir.path().join("cistella-prepare-test-session-1");
    assert!(path.exists(), "lease file present after create");
    let first = std::fs::read_to_string(&path).expect("read lease");
    assert!(first.starts_with("driver "), "got: {first}");
    lease.record_guest(1234, 1234).expect("guest records");
    let second = std::fs::read_to_string(&path).expect("read lease");
    assert!(second.contains("\nguest 1234 1234\n"), "got: {second}");
    drop(lease);
    assert!(!path.exists(), "drop removes the lease");
}

#[test]
fn lease_create_existing_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _first = PrepareLease::create(dir.path(), "test-session-1").expect("first creates");
    let error = PrepareLease::create(dir.path(), "test-session-1").expect_err("EEXIST must refuse");
    assert!(
        error
            .to_string()
            .contains("cistella-prepare-test-session-1"),
        "got: {error}"
    );
}

#[test]
fn lease_bad_session_id_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    for bad in ["", "../escape", "a/b", "has space", "semi;colon"] {
        let error = PrepareLease::create(dir.path(), bad).expect_err("bad id must refuse");
        assert!(
            error.to_string().contains("framework-minted session id"),
            "id {bad:?} got: {error}"
        );
    }
}

#[test]
fn lease_retain_survives_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease = PrepareLease::create(dir.path(), "test-session-1").expect("lease creates");
    let path = dir.path().join("cistella-prepare-test-session-1");
    lease.retain();
    assert!(path.exists(), "retained lease survives drop");
}
