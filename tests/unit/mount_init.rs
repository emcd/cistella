//! Scoped nested-mountpoint pre-create pins.

use std::os::unix::fs::PermissionsExt;

use cistella::mount::{MountMode, MountTriple};

/// Owner-only tmpdir base plus trusted subdir: tempfile and
/// create_dir honor the host umask, so a permissive build-host
/// umask (e.g. 0002) would leave group-writable dirs that
/// traversal trust (lifetime test) refuses.
fn test_tree() -> tempfile::TempDir {
    let tree = tempfile::tempdir().expect("tree");
    std::fs::set_permissions(tree.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod base");
    tree
}

fn test_subdir(tree: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let path = tree.path().join(name);
    std::fs::create_dir(&path).expect("mkdir subdir");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn precreate_nested_mountpoints_seeds_parent_source() {
    use cistella::mount_init::precreate_nested_mountpoints;
    let tree = test_tree();
    let host = tree.path().to_string_lossy().to_string();
    // Slice parent exists (prepare minted it) and is a declared
    // cleanup root; the scratch mountpoint does not yet — podman
    // would create it as container-root without the pre-create.
    test_subdir(&tree, "slice");
    let roots = [format!("{host}/slice")];
    let triples = [
        MountTriple {
            host_source: format!("{host}/slice"),
            container_target: "/tmp".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/scratch"),
            container_target: "/tmp/scratch".to_string(),
            mode: MountMode::Rw,
        },
    ];
    precreate_nested_mountpoints(&triples, &roots).expect("pre-create");
    assert!(
        tree.path().join("slice/scratch").is_dir(),
        "mountpoint pre-exists seat-owned"
    );
    // Missing parents skip (validated elsewhere); top-level
    // mounts need nothing.
    let orphan = MountTriple {
        host_source: "/nonexistent-parent-xyz/source".to_string(),
        container_target: "/data".to_string(),
        mode: MountMode::Rw,
    };
    let nested_orphan = MountTriple {
        host_source: format!("{host}/other"),
        container_target: "/data/sub".to_string(),
        mode: MountMode::Rw,
    };
    precreate_nested_mountpoints(&[orphan, nested_orphan], &roots).expect("orphan skips");
    assert!(
        !tree.path().join("other").exists(),
        "no tree created under a missing parent"
    );
    // Nesting under an existing source outside the declared
    // roots skips: no writes into user source trees (podman
    // legacy behavior there, unchanged).
    test_subdir(&tree, "work");
    let work = MountTriple {
        host_source: format!("{host}/work"),
        container_target: "/work".to_string(),
        mode: MountMode::Rw,
    };
    let work_child = MountTriple {
        host_source: format!("{host}/work-bind"),
        container_target: "/work/sub".to_string(),
        mode: MountMode::Rw,
    };
    precreate_nested_mountpoints(&[work, work_child], &roots).expect("out-of-scope skips");
    assert!(
        !tree.path().join("work/sub").exists(),
        "no mountpoint outside cleanup roots"
    );
}

#[test]
fn precreate_refuses_planted_symlink() {
    use cistella::mount_init::precreate_nested_mountpoints;
    let tree = test_tree();
    let host = tree.path().to_string_lossy().to_string();
    test_subdir(&tree, "slice");
    // Guest plants a symlink where the mountpoint will land,
    // pointing outside the admitted source.
    let outside = test_subdir(&tree, "outside");
    std::os::unix::fs::symlink(&outside, tree.path().join("slice/scratch")).expect("plant link");
    let roots = [format!("{host}/slice")];
    let triples = [
        MountTriple {
            host_source: format!("{host}/slice"),
            container_target: "/tmp".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/scratch"),
            container_target: "/tmp/scratch".to_string(),
            mode: MountMode::Rw,
        },
    ];
    let error = precreate_nested_mountpoints(&triples, &roots).expect_err("link refuses");
    assert!(
        error.to_string().contains("scratch"),
        "names the link, got: {error}"
    );
    assert!(
        !outside.join("scratch").exists() && outside.read_dir().expect("read").next().is_none(),
        "nothing created through the link"
    );
}

#[test]
fn precreated_mountpoint_dies_with_cleanup_root() {
    use cistella::cleanup::{reap_cleanup, record_cleanup};
    use cistella::mount_init::precreate_nested_mountpoints;
    // Lifetime contract: pre-create runs under declared roots,
    // so a later planning refusal strands nothing — teardown
    // (or gc) reaps the mountpoint with its root.
    let base = test_tree();
    let tree = test_tree();
    let host = tree.path().to_string_lossy().to_string();
    test_subdir(&tree, "slice");
    let roots = [format!("{host}/slice")];
    record_cleanup(base.path(), "test-session-9", &roots).expect("record admits");
    let triples = [
        MountTriple {
            host_source: format!("{host}/slice"),
            container_target: "/tmp".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/scratch"),
            container_target: "/tmp/scratch".to_string(),
            mode: MountMode::Rw,
        },
    ];
    precreate_nested_mountpoints(&triples, &roots).expect("pre-create");
    assert!(
        tree.path().join("slice/scratch").is_dir(),
        "mountpoint made"
    );
    let litter = reap_cleanup(base.path(), "test-session-9");
    assert!(litter.is_empty(), "clean reap, got: {litter:?}");
    assert!(
        !tree.path().join("slice").exists(),
        "root dies with its mountpoint"
    );
}
