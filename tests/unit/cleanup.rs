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

#[test]
fn lease_roundtrip_parses() {
    use cistella::cleanup::read_lease;
    use cistella::lock::PrepareLease;
    let dir = tempfile::tempdir().expect("tempdir");
    // Absent lease reads as none (no prepare recorded).
    assert!(
        read_lease(dir.path(), "test-session-1")
            .expect("absent reads")
            .is_none()
    );
    let mut lease = PrepareLease::create(dir.path(), "test-session-1").expect("lease creates");
    lease.record_guest(1111, 1111).expect("first records");
    lease.record_guest(2222, 3333).expect("second appends");
    let state = read_lease(dir.path(), "test-session-1")
        .expect("present reads")
        .expect("some");
    assert_eq!(state.guests, vec![(1111, 1111), (2222, 3333)]);
    assert_eq!(state.driver_pid, std::process::id());
}

#[test]
fn malformed_lease_reads_as_error() {
    use cistella::cleanup::read_lease;
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, content) in [
        ("no-driver", "guest 1 1\n"),
        ("bad-driver-pid", "driver xyz\n"),
        ("bad-guest", "driver 1\nguest 1\n"),
        ("trailing-junk", "driver 1\nextra stuff here\n"),
        ("empty", ""),
    ] {
        let path = dir.path().join(format!("cistella-prepare-{name}"));
        std::fs::write(&path, content).expect("write fixture");
        let error = read_lease(dir.path(), name).expect_err("malformed must err");
        assert!(error.to_string().contains("malformed"), "got: {error}");
    }
}

#[test]
fn liveness_predicates_observe_only() {
    use cistella::cleanup::{group_dead, pid_alive};
    let me = std::process::id();
    assert!(pid_alive(me), "own pid is alive");
    assert!(pid_alive(0), "pid zero reads alive (never reapable)");
    assert!(!group_dead(me), "own process group is alive");
    // A reaped child in its own group: proven absent (ESRCH-only
    // establishment — any other outcome would read alive).
    let mut child = std::process::Command::new("/bin/true");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setpgid in the child before exec is the standard
        // process-group setup (same call the guest spawn uses).
        unsafe {
            child.pre_exec(|| {
                if nix::libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
    }
    let mut child = child.spawn().expect("spawn true");
    let pid = child.id();
    child.wait().expect("reap child");
    assert!(group_dead(pid), "reaped lone group is dead");
}

#[test]
fn slice_session_parses_or_skips() {
    use cistella::cleanup::parse_slice_session;
    assert_eq!(
        parse_slice_session("cistella-tmp-abc123-456-789"),
        Some("abc123".to_string())
    );
    assert_eq!(
        parse_slice_session("cistella-tmp-test-session-1-123-456789"),
        Some("test-session-1".to_string())
    );
    for bad in [
        "cistella-tmp-abc123",
        "cistella-tmp-abc123-456",
        "cistella-tmp-abc123-xyz-789",
        "cistella-tmp--123-456",
        "other-prefix-abc-123-456",
        "cistella-tmp-ABC-123-456",
        "cistella-tmp",
    ] {
        assert_eq!(parse_slice_session(bad), None, "skips {bad:?}");
    }
}

#[test]
fn seat_temp_root_rule() {
    use cistella::cleanup::seat_temp_root_from;
    assert_eq!(seat_temp_root_from(None), std::path::PathBuf::from("/tmp"));
    assert_eq!(
        seat_temp_root_from(Some("")),
        std::path::PathBuf::from("/tmp")
    );
    assert_eq!(
        seat_temp_root_from(Some("relative/dir")),
        std::path::PathBuf::from("/tmp")
    );
    assert_eq!(
        seat_temp_root_from(Some("/tmp/scratch")),
        std::path::PathBuf::from("/tmp/scratch")
    );
}
