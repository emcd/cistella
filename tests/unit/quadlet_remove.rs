//! Concurrent-remover unit-file unlink: NotFound converges, other IO faults propagate.
//!
//! Pins the `quadlet-remove-notfound-tolerant` repair. Each test drives
//! the pure `unlink_unit_file` seam with explicit tmpdir fixtures (no
//! process env, no systemd): the peer-remover interleave is reproduced
//! by deleting the fixture between observation and unlink, exactly the
//! old `exists()` → peer-unlinks → unlink-ENOENT sequence that mapped
//! to a `Runtime` error on baseline. Fixture perms are explicit so a
//! permissive build-host umask cannot fail-closed the suite.

use std::os::unix::fs::PermissionsExt;

use cistella::isolators::quadlet::unlink_unit_file;

fn test_base() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

fn write_unit(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, b"[Container]\n").expect("write fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    path
}

/// The losing remover of a concurrent-removal race converges: the peer
/// unlinked between this caller's observation and its unlink, and the
/// resulting `NotFound` is converged absence — `Ok(false)`, no error.
#[test]
fn peer_removed_unit_converges_absent() {
    let dir = test_base();
    let path = write_unit(&dir, "cistella-peer.container");
    assert!(path.exists(), "fixture observed present (old exists() arm)");
    // Peer remover wins the race here.
    std::fs::remove_file(&path).expect("peer unlink");
    assert!(
        !unlink_unit_file(&path).expect("loser converges"),
        "already-absent unit is converged success, not an error"
    );
}

/// An absent unit with no peer involved is equally converged: removing
/// an absent unit is the converged state, never an error.
#[test]
fn absent_unit_converges_without_error() {
    let dir = test_base();
    let path = dir.path().join("cistella-never-created.container");
    assert!(
        !unlink_unit_file(&path).expect("absent converges"),
        "absent unit reports no unlink performed"
    );
}

/// A present unit unlinks and reports that this caller performed the
/// unlink — the caller that owns `daemon-reload`.
#[test]
fn present_unit_unlinks_and_reports_ownership() {
    let dir = test_base();
    let path = write_unit(&dir, "cistella-winner.container");
    assert!(
        unlink_unit_file(&path).expect("unlink succeeds"),
        "present unit reports this caller unlinked"
    );
    assert!(!path.exists(), "unit file actually removed");
}

/// Non-NotFound IO failures still propagate: unlinking a directory
/// fails closed instead of being misread as converged absence.
#[test]
fn directory_unlink_propagates_error() {
    let dir = test_base();
    let sub = dir.path().join("not-a-file.container");
    std::fs::create_dir(&sub).expect("mkdir fixture");
    let result = unlink_unit_file(&sub);
    assert!(
        result.is_err(),
        "directory unlink must error, got: {result:?}"
    );
    assert!(sub.exists(), "failed unlink leaves the path alone");
}

/// Negative control for the repair: the pre-repair error mapping
/// (`remove_file` with EVERY error mapped to failure) run against the
/// peer-removal interleave yields the failure the repair eliminates.
/// Single-threaded reproduction of the loser's view: the file was
/// observed present, the peer unlinked, and this caller's unlink hits
/// ENOENT — the old arm turns it into an error where
/// `unlink_unit_file` converges. Paired with
/// `peer_removed_unit_converges_absent` (same interleave, new seam
/// converges), the pair is the regression proof: old arm errors,
/// new seam succeeds, on the identical filesystem facts.
#[test]
fn baseline_error_mapping_fails_on_peer_removal() {
    let dir = test_base();
    let path = write_unit(&dir, "cistella-baseline.container");
    assert!(path.exists(), "fixture observed present (old exists() arm)");
    // Peer remover wins the race here.
    std::fs::remove_file(&path).expect("peer unlink");
    // Pre-repair arm, verbatim shape: every unlink error is a failure.
    let result: Result<(), String> =
        std::fs::remove_file(&path).map_err(|e| format!("remove {path:?}: {e}"));
    let error = result.expect_err("old mapping must fail on peer-removed unit");
    assert!(
        error.contains("No such file"),
        "old failure is the ENOENT mode, got: {error}"
    );
}

/// Permission failures still propagate: a unit file this user cannot
/// remove is a real failure, never converged absence.
#[test]
fn permission_denied_unlink_propagates_error() {
    let dir = test_base();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).expect("mkdir fixture");
    let path = locked.join("cistella-locked.container");
    std::fs::write(&path, b"[Container]\n").expect("write fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let result = unlink_unit_file(&path);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(
        result.is_err(),
        "permission-denied unlink must error, got: {result:?}"
    );
}
