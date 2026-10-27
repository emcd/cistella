//! Backend-free pins for recursive anchored state-dir creation.
//!
//! A fresh seat has no state chain yet; `open_state_dir_all`
//! creates missing ancestors with the same discipline as
//! `open_state_dir` (0700-at-creation, nofollow open,
//! seat-or-root ownership). A symlink at any level refuses the
//! whole open and nothing is ever created through a link.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use cistella::guest_anchor::open_state_dir_all;

fn mode(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path).expect("stat").mode() & 0o777
}

#[test]
fn missing_chain_is_created_with_discipline() {
    // None of the chain exists: every level is created 0700 and
    // the open directory is usable for anchored writes.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let deep = root.path().join("x").join("y").join("z");
    let anchored = open_state_dir_all(&deep).expect("fresh chain opens");
    for level in [
        &root.path().join("x"),
        &root.path().join("x").join("y"),
        &deep,
    ] {
        assert!(level.is_dir(), "level created: {}", level.display());
        assert_eq!(mode(level), 0o700, "level hardened: {}", level.display());
    }
    let mut file = anchored.create_file("probe").expect("anchored write");
    use std::io::Write;
    file.write_all(b"anchored").expect("write");
    drop(file);
    let back = anchored
        .read_file("probe", 1024)
        .expect("anchored read")
        .expect("present");
    assert_eq!(back, b"anchored");
}

#[test]
fn existing_tree_opens_unchanged() {
    // A fully existing tree opens without creating anything: the
    // target keeps the long-standing harden-to-0700 behavior of
    // `open_state_dir`, while ancestors are verified, not
    // modified.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let anchor = root.path().join("anchor");
    std::fs::create_dir(&anchor).expect("anchor");
    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o755)).expect("perms");
    let root_before = mode(root.path());
    open_state_dir_all(&anchor).expect("existing dir opens");
    assert_eq!(mode(&anchor), 0o700, "target adopted and hardened");
    assert_eq!(mode(root.path()), root_before, "ancestor untouched");
    let entries: Vec<_> = std::fs::read_dir(root.path()).expect("list").collect();
    assert_eq!(entries.len(), 1, "nothing created beside the anchor");
}

#[test]
fn symlink_leaf_refuses() {
    // A symlink at the target refuses, matching the
    // non-recursive open.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let real = root.path().join("real");
    std::fs::create_dir(&real).expect("real dir");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    assert!(open_state_dir_all(&link).is_err(), "symlink target refused");
}

#[test]
fn symlink_ancestor_refuses_without_creating_through() {
    // A symlink mid-chain refuses the whole open, and nothing is
    // created on the far side of the link.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let real = root.path().join("real");
    std::fs::create_dir(&real).expect("real dir");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let err = open_state_dir_all(&link.join("child")).expect_err("link mid-chain refuses");
    assert!(
        err.contains("must not be a symlink"),
        "refusal names the symlink: {err}"
    );
    assert!(
        !real.join("child").exists(),
        "nothing created through the link"
    );
}

#[test]
fn file_leaf_refuses() {
    // A regular file at the target is not a directory.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let file = root.path().join("file");
    std::fs::write(&file, b"x").expect("file");
    assert!(open_state_dir_all(&file).is_err(), "file refused");
}

#[test]
fn missing_chain_hardens_pre_existing_umask_modes() {
    // Levels the opener creates land 0700 even under a permissive
    // umask; pre-existing ancestors keep their own mode.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let anchor = root.path().join("anchor");
    std::fs::create_dir(&anchor).expect("anchor");
    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o755)).expect("perms");
    let deep = anchor.join("x").join("y");
    open_state_dir_all(&deep).expect("chain opens");
    assert_eq!(mode(&anchor), 0o755, "ancestor mode untouched");
    assert_eq!(mode(&anchor.join("x")), 0o700, "created level hardened");
    assert_eq!(mode(&deep), 0o700, "target hardened");
}

#[test]
fn intermediate_symlink_with_descendants_refuses_missing_leaf() {
    // F1: root/link -> outside, outside/existing an ordinary
    // owned directory. Requesting root/link/existing/new must
    // refuse at the link — the old walk accepted the reached
    // directory and created outside the intended root.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).expect("perms");
    let existing = outside.join("existing");
    std::fs::create_dir(&existing).expect("existing");
    std::fs::write(outside.join("sentinel"), b"untouched").expect("sentinel");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    let err = open_state_dir_all(&link.join("existing").join("new")).expect_err("link refuses");
    assert!(
        err.contains("must not be a symlink"),
        "refusal names the symlink: {err}"
    );
    assert!(
        !existing.join("new").exists(),
        "nothing created outside the intended root"
    );
    assert_eq!(mode(&outside), 0o755, "outside mode unchanged");
    assert_eq!(
        std::fs::read(outside.join("sentinel")).expect("sentinel"),
        b"untouched",
        "outside contents unchanged"
    );
}

#[test]
fn intermediate_symlink_with_descendants_refuses_existing_leaf() {
    // F1 existing-leaf: the whole target already exists behind
    // the intermediate link. The old walk accepted it (and
    // fchmodded it); the anchored walk must refuse with outside
    // modes untouched.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).expect("perms");
    let existing = outside.join("existing");
    std::fs::create_dir(&existing).expect("existing");
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).expect("perms");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    let err = open_state_dir_all(&link.join("existing")).expect_err("link refuses");
    assert!(
        err.contains("must not be a symlink"),
        "refusal names the symlink: {err}"
    );
    assert_eq!(mode(&outside), 0o755, "outside mode unchanged");
    assert_eq!(mode(&existing), 0o755, "reached directory not adopted");
}

#[test]
fn replaced_parent_symlink_refuses_without_traversal() {
    // F2 steady state: the parent the walk would descend through
    // is a symlink — what a checked-parent replacement race
    // would leave behind. The descriptor-anchored walk refuses
    // at the link itself, so the outside sentinel is untouched.
    let root = tempfile::TempDir::new().expect("tmpdir");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::write(outside.join("sentinel"), b"untouched").expect("sentinel");
    let victim = root.path().join("victim");
    std::os::unix::fs::symlink(&outside, &victim).expect("symlink");
    let err = open_state_dir_all(&victim.join("new")).expect_err("link refuses");
    assert!(
        err.contains("must not be a symlink"),
        "refusal names the symlink: {err}"
    );
    assert!(
        !outside.join("new").exists(),
        "no traversal through the replaced parent"
    );
    assert_eq!(
        std::fs::read(outside.join("sentinel")).expect("sentinel"),
        b"untouched",
        "outside sentinel unchanged"
    );
}

#[test]
fn checked_parent_replacement_stays_on_held_identity() {
    // F2 deterministic seam: after the walk verifies parent
    // root/a and before it creates b, the hook renames a away
    // and plants a symlink at its place pointing outside. The
    // descriptor-anchored walk must stay on the held directory
    // identity: b/c land in the renamed original and the
    // returned handle writes there — never through the link.
    use std::os::fd::RawFd;
    let root = tempfile::TempDir::new().expect("tmpdir");
    let a = root.path().join("a");
    std::fs::create_dir(&a).expect("parent");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::write(outside.join("sentinel"), b"untouched").expect("sentinel");
    let anchored = cistella::guest_anchor::open_state_dir_all_with_hook(
        &a.join("b").join("c"),
        Some(&|parent: &std::path::Path, _fd: RawFd| {
            if parent == a {
                let orig = root.path().join("a_orig");
                std::fs::rename(&a, &orig).expect("swap parent away");
                std::os::unix::fs::symlink(&outside, &a).expect("plant link");
            }
        }),
    )
    .expect("walk stays on the held identity");
    let mut marker = anchored
        .create_file("marker")
        .expect("write through handle");
    use std::io::Write;
    marker.write_all(b"held").expect("marker");
    drop(marker);
    let orig = root.path().join("a_orig");
    assert_eq!(
        std::fs::read(orig.join("b").join("c").join("marker")).expect("marker"),
        b"held",
        "creation and writes went to the held original, not the link"
    );
    assert!(
        !outside.join("b").exists(),
        "no traversal through the replacement link"
    );
    assert_eq!(
        std::fs::read(outside.join("sentinel")).expect("sentinel"),
        b"untouched",
        "outside sentinel unchanged"
    );
}
