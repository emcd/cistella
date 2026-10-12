//! Durable prepare leases: create/update/remove lifecycle, all
//! against explicit tmpdir bases (no process env — parallel-safe
//! and hermetic).

use cistella::lock::PrepareLease;

#[test]
fn lease_create_record_remove_lifecycle() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut lease = PrepareLease::create(dir.path(), "test-session-1").expect("lease creates");
    let path =
        cistella::lock::PrepareLease::state_dir(dir.path()).join("cistella-prepare-test-session-1");
    assert!(path.exists(), "lease file present after create");
    let first = std::fs::read_to_string(&path).expect("read lease");
    assert!(first.starts_with("driver "), "got: {first}");
    lease.record_guest(1234, 1234).expect("guest records");
    lease
        .record_guest(5678, 5678)
        .expect("second guest appends");
    let second = std::fs::read_to_string(&path).expect("read lease");
    assert!(
        second.contains("\nguest 1234 1234\nguest 5678 5678\n"),
        "got: {second}"
    );
    drop(lease);
    assert!(!path.exists(), "drop removes the lease");
}

#[test]
fn lease_create_existing_refuses() {
    let dir = tempfile::tempdir().expect("tmpdir");
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
    let dir = tempfile::tempdir().expect("tmpdir");
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
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut lease = PrepareLease::create(dir.path(), "test-session-1").expect("lease creates");
    let path =
        cistella::lock::PrepareLease::state_dir(dir.path()).join("cistella-prepare-test-session-1");
    lease.retain();
    assert!(path.exists(), "retained lease survives drop");
}

#[test]
fn lease_pending_roundtrip() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut lease = PrepareLease::create(dir.path(), "test-session-1").expect("lease creates");
    lease.mark_pending().expect("pending marks");
    let path =
        cistella::lock::PrepareLease::state_dir(dir.path()).join("cistella-prepare-test-session-1");
    let text = std::fs::read_to_string(&path).expect("read lease");
    assert!(text.contains("\npending\n"), "got: {text}");
    lease.record_guest(1111, 1111).expect("record clears");
    let text = std::fs::read_to_string(&path).expect("read lease");
    assert!(!text.contains("pending"), "pending cleared, got: {text}");
    assert!(text.contains("\nguest 1111 1111\n"), "got: {text}");
}

#[test]
fn lease_symlink_at_path_refuses_without_touching_target() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let statedir = cistella::lock::PrepareLease::state_dir(dir.path());
    std::fs::create_dir(&statedir).expect("mkdir state dir");
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, b"precious").expect("write victim");
    std::os::unix::fs::symlink(&victim, statedir.join("cistella-prepare-test-session-1"))
        .expect("plant symlink");
    let error = PrepareLease::create(dir.path(), "test-session-1")
        .expect_err("symlink at lease path must refuse");
    assert!(
        error.to_string().contains("already exists") || error.to_string().contains("symlink"),
        "got: {error}"
    );
    assert_eq!(std::fs::read(&victim).expect("victim intact"), b"precious");
}

#[test]
fn state_dir_hardens_mode_and_refuses_foreign_owner() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tmpdir");
    let statedir = cistella::lock::PrepareLease::state_dir(dir.path());
    std::fs::create_dir(&statedir).expect("mkdir state dir");
    std::fs::set_permissions(&statedir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let _lease = PrepareLease::create(dir.path(), "test-session-1").expect("lease creates");
    assert_eq!(
        std::fs::metadata(&statedir)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "pre-existing dir hardens to 0700"
    );
    // SAFETY: getuid is inherently safe.
    if unsafe { nix::libc::getuid() } != 0 {
        eprintln!("skip: foreign-owner state dir needs root (chown)");
        return;
    }
    std::os::unix::fs::chown(&statedir, Some(65534), None).expect("chown nobody");
    let error = PrepareLease::create(dir.path(), "test-session-2")
        .expect_err("foreign state dir must refuse");
    assert!(
        error.to_string().contains("seat- or root-owned"),
        "got: {error}"
    );
}

#[test]
fn state_dir_is_per_seat() {
    // Per-seat namespace: one seat creating the fallback first
    // never locks another seat uid out of its own state.
    let dir = tempfile::tempdir().expect("tmpdir");
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    assert_eq!(
        cistella::lock::PrepareLease::state_dir(dir.path()),
        dir.path().join(format!("cistella-state-{seat}"))
    );
}
