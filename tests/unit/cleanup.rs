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
    let statedir = state_subdir(&dir);
    for (name, content) in [
        ("no-driver", "guest 1 1\n"),
        ("bad-driver-pid", "driver xyz\n"),
        ("bad-guest", "driver 1\nguest 1\n"),
        ("trailing-junk", "driver 1\nextra stuff here\n"),
        ("empty", ""),
    ] {
        let path = statedir.join(format!("cistella-prepare-{name}"));
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
    // Own process GROUP (not pid — under plain `cargo test` the
    // test thread shares its parent's group, while nextest runs
    // each test as its own group leader).
    let pgid =
        u32::try_from(nix::unistd::getpgid(None).expect("getpgid").as_raw()).expect("pgid fits");
    assert!(!group_dead(pgid), "own process group is alive");
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

fn state_subdir(base: &tempfile::TempDir) -> std::path::PathBuf {
    let statedir = cistella::lock::PrepareLease::state_dir(base.path());
    std::fs::create_dir_all(&statedir).expect("mkdir state dir");
    statedir
}

/// A certainly-dead pid and a certainly-dead lone process group
/// (spawned, reaped): deterministic without privilege.
fn dead_pids() -> (u32, u32) {
    let mut driver = std::process::Command::new("/bin/true")
        .spawn()
        .expect("spawn true");
    let driver_pid = driver.id();
    driver.wait().expect("reap driver");
    let mut guest = std::process::Command::new("/bin/true");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setpgid in the child before exec is standard.
        unsafe {
            guest.pre_exec(|| {
                if nix::libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
    }
    let mut guest = guest.spawn().expect("spawn guest");
    let guest_pid = guest.id();
    guest.wait().expect("reap guest");
    (driver_pid, guest_pid)
}

#[test]
fn forged_identity_record_refuses() {
    // Hand-forged state JSON claims a bogus dev+ino for a real
    // seat-owned dir: reap must refuse (identity mismatch), and
    // the directory survives.
    let dir = tempfile::tempdir().expect("tempdir");
    let statedir = state_subdir(&dir);
    let root = seat_dir(&dir, "slice");
    let forged = serde_json::json!([{
        "path": root.to_string_lossy(),
        "dev": 999001,
        "ino": 999002,
        "parent_dev": 999003,
        "parent_ino": 999004,
    }]);
    std::fs::write(
        statedir.join("cistella-cleanup-forged-1"),
        serde_json::to_vec(&forged).expect("encode"),
    )
    .expect("plant forged record");
    let litter = reap_cleanup(dir.path(), "forged-1");
    assert_eq!(litter.len(), 1, "one refusal, got: {litter:?}");
    assert!(litter[0].contains("identity"), "got: {}", litter[0]);
    assert!(root.exists(), "forged record reaps nothing");
}

#[test]
fn parent_swap_refuses() {
    // Move the admitted root under a fresh parent: root identity
    // matches but the parent association does not — refuse.
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = seat_dir(&dir, "parent");
    let root = parent.join("slice");
    std::fs::create_dir(&root).expect("mkdir root");
    record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect("record admits");
    let parent2 = dir.path().join("parent2");
    std::fs::rename(&parent, &parent2).expect("rename parent aside");
    std::fs::create_dir(&parent).expect("fresh parent");
    std::fs::rename(parent2.join("slice"), parent.join("slice")).expect("move root back");
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert_eq!(litter.len(), 1, "one refusal, got: {litter:?}");
    assert!(litter[0].contains("parent"), "got: {}", litter[0]);
    assert!(parent.join("slice").exists(), "moved root survives");
}

#[test]
fn world_writable_parent_refuses() {
    // Mutating path operations happen only under trusted
    // parents: a world-writable non-sticky parent lets untrusted
    // actors swap entries, so traversal refuses the whole entry
    // (fail-closed litter, retryable).
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = seat_dir(&dir, "parent");
    let root = parent.join("slice");
    std::fs::create_dir(&root).expect("mkdir root");
    record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect("record admits");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert_eq!(litter.len(), 1, "one refusal, got: {litter:?}");
    assert!(litter[0].contains("world-writable"), "got: {}", litter[0]);
    assert!(root.exists(), "refused root survives");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn gone_root_is_complete_not_litter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = seat_dir(&dir, "slice");
    record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect("record admits");
    std::fs::remove_dir(&root).expect("remove root out of band");
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert!(litter.is_empty(), "gone reads complete, got: {litter:?}");
    // Retried states converge: the record is gone with its root.
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert!(litter.is_empty(), "retry clean, got: {litter:?}");
}

#[test]
fn state_symlink_refuses_without_touching_target() {
    // A symlink at the state DIRECTORY refuses (ELOOP at the
    // nofollow open — never created through, chmodded, or
    // operated in).
    let dir = tempfile::tempdir().expect("tempdir");
    let elsewhere = tempfile::tempdir().expect("tempdir");
    std::os::unix::fs::symlink(
        elsewhere.path(),
        cistella::lock::PrepareLease::state_dir(dir.path()),
    )
    .expect("plant dir symlink");
    let victim = elsewhere.path().join("victim.txt");
    std::fs::write(&victim, b"precious").expect("write victim");
    let root = seat_dir(&dir, "slice");
    let error = record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect_err("symlinked state dir must refuse");
    assert!(error.to_string().contains("symlink"), "got: {error}");
    assert_eq!(std::fs::read(&victim).expect("victim intact"), b"precious");
    assert!(root.exists(), "refusal stages nothing");
}

#[test]
fn state_file_symlink_replaces_link_safely() {
    // A symlink at the state FILE path itself is replaced (never
    // traversed): renameat swaps the link for the record, and the
    // link target is never written through.
    let dir = tempfile::tempdir().expect("tempdir");
    let statedir = state_subdir(&dir);
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, b"precious").expect("write victim");
    std::os::unix::fs::symlink(&victim, statedir.join("cistella-cleanup-test-session-1"))
        .expect("plant file symlink");
    let root = seat_dir(&dir, "slice");
    record_cleanup(
        dir.path(),
        "test-session-1",
        &[root.to_string_lossy().to_string()],
    )
    .expect("record replaces the link");
    assert_eq!(std::fs::read(&victim).expect("victim intact"), b"precious");
    let state = statedir.join("cistella-cleanup-test-session-1");
    assert!(
        state.is_file() && !state.is_symlink(),
        "state path is a real file now"
    );
    let litter = reap_cleanup(dir.path(), "test-session-1");
    assert!(litter.is_empty(), "replaced record reaps, got: {litter:?}");
    assert!(!root.exists(), "slice reaped");
}

#[test]
fn pending_lease_skips_sweep() {
    use cistella::cleanup::reap_tempdir_orphans_in;
    use std::collections::HashSet;
    // Pending marker with a dead driver: a guest may exist
    // unrecorded — gc skips (never assumes), lease persists.
    let base = tempfile::tempdir().expect("tempdir");
    let root = tempfile::tempdir().expect("tempdir");
    let statedir = state_subdir(&base);
    let (driver_pid, _) = dead_pids();
    let session = "livetest-pending-1";
    let slice = root.path().join(format!("cistella-tmp-{session}-111-222"));
    std::fs::create_dir(&slice).expect("mkdir slice");
    std::fs::write(
        statedir.join(format!("cistella-prepare-{session}")),
        format!("driver {driver_pid}\npending\n"),
    )
    .expect("write pending lease");
    let removed = reap_tempdir_orphans_in(root.path(), base.path(), &HashSet::new());
    assert!(removed.is_empty(), "pending skips, got: {removed:?}");
    assert!(slice.exists(), "pending slice survives");
    assert!(
        statedir
            .join(format!("cistella-prepare-{session}"))
            .exists(),
        "pending lease persists"
    );
}

#[test]
fn stale_lease_and_orphan_go_together() {
    use cistella::cleanup::reap_tempdir_orphans_in;
    use std::collections::HashSet;
    // Dead driver plus dead recorded group: the slice reaps and
    // the stale lease file goes with it.
    let base = tempfile::tempdir().expect("tempdir");
    let root = tempfile::tempdir().expect("tempdir");
    let statedir = state_subdir(&base);
    let (driver_pid, guest_pid) = dead_pids();
    let session = "livetest-stale-1";
    let slice = root.path().join(format!("cistella-tmp-{session}-111-222"));
    std::fs::create_dir(&slice).expect("mkdir slice");
    std::fs::set_permissions(&slice, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::write(
        statedir.join(format!("cistella-prepare-{session}")),
        format!("driver {driver_pid}\nguest {guest_pid} {guest_pid}\n"),
    )
    .expect("write stale lease");
    let removed = reap_tempdir_orphans_in(root.path(), base.path(), &HashSet::new());
    assert_eq!(
        removed,
        vec![format!("cistella-tmp-{session}-111-222")],
        "got: {removed:?}"
    );
    assert!(!slice.exists(), "orphan reaped");
    assert!(
        !statedir
            .join(format!("cistella-prepare-{session}"))
            .exists(),
        "stale lease removed"
    );
}

#[test]
fn guestless_dead_lease_proceeds() {
    use cistella::cleanup::reap_tempdir_orphans_in;
    use std::collections::HashSet;
    // No guest lines, no pending marker, dead driver: spawn never
    // happened (pending always commits first), so nothing stands
    // in the way — slice reaps, lease goes with it.
    let base = tempfile::tempdir().expect("tempdir");
    let root = tempfile::tempdir().expect("tempdir");
    let statedir = state_subdir(&base);
    let (driver_pid, _) = dead_pids();
    let session = "livetest-guestless-1";
    let slice = root.path().join(format!("cistella-tmp-{session}-111-222"));
    std::fs::create_dir(&slice).expect("mkdir slice");
    std::fs::set_permissions(&slice, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::write(
        statedir.join(format!("cistella-prepare-{session}")),
        format!("driver {driver_pid}\n"),
    )
    .expect("write guestless lease");
    let removed = reap_tempdir_orphans_in(root.path(), base.path(), &HashSet::new());
    assert_eq!(removed.len(), 1, "got: {removed:?}");
    assert!(!slice.exists(), "orphan reaped");
}
