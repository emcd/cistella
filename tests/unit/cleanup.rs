//! Declared-root cleanup: admit, record, and FD-relative reap.
//!
//! All fixtures use explicit tempdir bases (no process env).
//! Ownership gates pass as the seat user; untrusted-owner arms
//! are root-conditional with documented skips.

use std::os::unix::fs::PermissionsExt;

use cistella::cleanup::{reap_cleanup, record_cleanup};

fn seat_dir(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::create_dir(&path).expect("mkdir fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn populated_tree_removes_fully_outside_target_survives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = seat_dir(&dir, "slice");
    // Harness litter: files, a subdir, and a nested symlink
    // pointing outside the slice.
    std::fs::write(root.join("file.txt"), b"data").expect("write file");
    let sub = root.join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    std::fs::write(sub.join("nested.txt"), b"nested").expect("write nested");
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"precious").expect("write outside");
    std::os::unix::fs::symlink(&outside, root.join("link-out")).expect("symlink");
    record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect("record admits");
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert!(litter.is_empty(), "clean reap, got: {litter:?}");
    assert!(!root.exists(), "root removed");
    assert_eq!(
        std::fs::read(&outside).expect("outside survives"),
        b"precious"
    );
}

#[test]
fn swapped_root_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = seat_dir(&dir, "slice");
    record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect("record admits");
    // Symlink swap: refuse without following.
    let target = dir.path().join("target");
    std::fs::create_dir(&target).expect("mkdir target");
    std::fs::remove_dir(&root).expect("remove original");
    std::os::unix::fs::symlink(&target, &root).expect("swap symlink");
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert_eq!(litter.len(), 1, "one refusal, got: {litter:?}");
    assert!(litter[0].contains("symlink"), "got: {}", litter[0]);
    assert!(target.exists(), "symlink target untouched");
    // Directory-for-directory swap: dev+ino mismatch refuses.
    std::fs::remove_file(&root).expect("remove link");
    std::fs::create_dir(&root).expect("replacement dir");
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert_eq!(litter.len(), 1, "one refusal, got: {litter:?}");
    assert!(litter[0].contains("identity"), "got: {}", litter[0]);
    assert!(root.exists(), "replacement left in place");
}

#[test]
fn undeclared_mounts_survive_teardown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = seat_dir(&dir, "project");
    // No declarations recorded: reap is a no-op (absent state).
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert!(litter.is_empty(), "got: {litter:?}");
    assert!(project.exists(), "undeclared contribution survives");
}

#[test]
fn non_directory_root_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").expect("write file");
    let error = record_cleanup(
        dir.path(),
        "test-session-1",
        &[file.to_string_lossy().to_string()],
    )
    .expect_err("file root must refuse at record");
    assert!(
        error.to_string().contains("must be a directory"),
        "got: {error}"
    );
}

#[test]
fn relative_declaration_and_bad_session_refuse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = record_cleanup(dir.path(), "test-session-1", &["relative/path".to_string()])
        .expect_err("relative must refuse");
    assert!(error.to_string().contains("absolute"), "got: {error}");
    let error = record_cleanup(dir.path(), "../escape", &["/tmp/x".to_string()])
        .expect_err("bad session must refuse");
    assert!(
        error.to_string().contains("framework-minted session id"),
        "got: {error}"
    );
}

#[test]
fn untrusted_root_owner_refuses() {
    // Needs privilege (chown a directory to another uid); without
    // it the arm is review-covered (same predicate shape as the
    // file and ancestor ownership gates).
    // SAFETY: getuid is inherently safe.
    if unsafe { nix::libc::getuid() } != 0 {
        eprintln!("skip: untrusted-owner root needs root (chown)");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let root = seat_dir(&dir, "slice");
    std::os::unix::fs::chown(&root, Some(65534), None).expect("chown nobody");
    let error = record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect_err("alien root must refuse");
    assert!(
        error.to_string().contains("seat- or root-owned"),
        "got: {error}"
    );
}
