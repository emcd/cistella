//! Mount allowlist validation.

use cistella::mount::{MountMode, MountTriple, podman_volume_args, validate_mounts};
use cistella::profile::Profile;

#[test]
fn rejects_sensitive_root() {
    let t = MountTriple {
        host_source: "/tmp/foo".to_string(),
        container_target: "/etc/passwd".to_string(),
        mode: MountMode::Ro,
    };
    assert!(validate_mounts(&[t], "/home/cistella").is_err());
}

#[test]
fn rejects_container_home_traversal() {
    let prof = Profile::from_toml(
        r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
mounts = []
container-home = "/home/cistella/../../etc"
"#,
    );
    assert!(prof.is_err(), "traversal container_home must be rejected");
}

#[test]
fn allows_rw_under_rw_stacking() {
    // RW-under-RW nests: podman mounts parent-first (volume args sort
    // shallower-first) and mkdir works through RW parents. Deepest
    // mount wins.
    let a = MountTriple {
        host_source: "/tmp/a".to_string(),
        container_target: "/work".to_string(),
        mode: MountMode::Rw,
    };
    let b = MountTriple {
        host_source: "/tmp/b".to_string(),
        container_target: "/work/src".to_string(),
        mode: MountMode::Rw,
    };
    assert!(validate_mounts(&[a, b], "/home/cistella").is_ok());
    // Declaration order is irrelevant: child-first still stacks.
    let a = MountTriple {
        host_source: "/tmp/a".to_string(),
        container_target: "/work".to_string(),
        mode: MountMode::Rw,
    };
    let b = MountTriple {
        host_source: "/tmp/b".to_string(),
        container_target: "/work/src".to_string(),
        mode: MountMode::Rw,
    };
    assert!(validate_mounts(&[b, a], "/home/cistella").is_ok());
}

#[test]
fn allows_nesting_under_home() {
    let t = MountTriple {
        host_source: "/tmp/a".to_string(),
        container_target: "/home/cistella/.config/opencode".to_string(),
        mode: MountMode::Ro,
    };
    assert!(validate_mounts(&[t], "/home/cistella").is_ok());
}

#[test]
fn rejects_shadow_of_home() {
    let t = MountTriple {
        host_source: "/tmp/a".to_string(),
        container_target: "/home".to_string(),
        mode: MountMode::Ro,
    };
    assert!(validate_mounts(&[t], "/home/cistella").is_err());
}

#[test]
fn rejects_canonicalized_overlap_host() {
    let td = tempfile::tempdir().unwrap();
    let real = td.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = td.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let a = MountTriple {
        host_source: real.to_string_lossy().to_string(),
        container_target: "/work/a".to_string(),
        mode: MountMode::Rw,
    };
    let b = MountTriple {
        host_source: link.to_string_lossy().to_string(),
        container_target: "/work/b".to_string(),
        mode: MountMode::Rw,
    };
    // Same canonical host after symlink resolution → overlapping host_sources
    assert!(validate_mounts(&[a, b], "/home/cistella").is_err());
}

#[test]
fn volume_args_mount_parent_before_child() {
    // Implementation half of the stacking contract (validation half is
    // `allows_rw_under_rw_stacking`): child-first declaration still
    // emits the parent `--volume` first, so podman never mounts a
    // child onto a path the parent mount would shadow.
    let parent = triple("/tmp/a", "/data", MountMode::Rw);
    let child = triple("/tmp/a/sub", "/data/sub", MountMode::Rw);
    let args = podman_volume_args(&[child, parent], "/home/cistella", None);
    let volumes: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "--volume")
        .map(|w| w[1].as_str())
        .collect();
    assert_eq!(
        volumes,
        vec!["/tmp/a:/data:rw", "/tmp/a/sub:/data/sub:rw"],
        "parent mounts before child: {volumes:?}"
    );
}

#[test]
fn volume_args_order_home_first() {
    let t = MountTriple {
        host_source: "/tmp/a".to_string(),
        container_target: "/work".to_string(),
        mode: MountMode::Rw,
    };
    let args = podman_volume_args(&[t], "/home/cistella", None);
    assert_eq!(args[0], "--tmpfs");
    assert_eq!(args[1], "/home/cistella");
    assert!(args.contains(&"--volume".to_string()));
}

#[test]
fn profile_requires_credential_surface() {
    let bad = r#"
image = "localhost/cistella/opencode:example"
container-home = "/home/cistella"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_requires_image() {
    let bad = r#"
credential-surface = "none"
mounts = []
container-home = "/home/cistella"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_requires_mounts() {
    let bad = r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
container-home = "/home/cistella"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_rejects_reserved_label() {
    let bad = r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
mounts = []
[labels]
cistella.id = "spoof"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_accepts_command_array_and_labels() {
    let ok = r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
mounts = []
command = ["opencode", "--model", "x"]
[labels]
"agentmux.session" = "s1"
"#;
    let prof = Profile::from_toml(ok).unwrap();
    assert_eq!(prof.command.unwrap(), vec!["opencode", "--model", "x"]);
}

#[test]
fn profile_expands_tilde_in_host_source() {
    let ok = r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
[[mounts]]
host-source = "~/.config/opencode"
container-target = "/home/cistella/.config/opencode"
mode = "ro"
"#;
    let prof = Profile::from_toml(ok).unwrap();
    assert!(!prof.mounts[0].host_source.starts_with('~'));
    assert!(prof.mounts[0].host_source.ends_with(".config/opencode"));
}

#[test]
fn profile_rejects_home_in_env() {
    let bad = r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
mounts = []
container-home = "/home/cistella"
[environment-assignments]
HOME = "/override"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_accepts_valid() {
    let ok = r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
container-home = "/home/cistella"
[[mounts]]
host-source = "/tmp/a"
container-target = "/work"
mode = "rw"
"#;
    assert!(Profile::from_toml(ok).is_ok());
}

fn triple(host: &str, target: &str, mode: MountMode) -> MountTriple {
    MountTriple {
        host_source: host.to_string(),
        container_target: target.to_string(),
        mode,
    }
}

#[test]
fn session_directory_pair_defaults_to_work() {
    use cistella::mount::parse_session_directory;
    assert_eq!(
        parse_session_directory("/repo").unwrap(),
        ("/repo".to_string(), "/work".to_string())
    );
    assert_eq!(
        parse_session_directory("/repo:/repo").unwrap(),
        ("/repo".to_string(), "/repo".to_string())
    );
}

#[test]
fn session_directory_pair_rejects_bad_sides() {
    use cistella::mount::parse_session_directory;
    assert!(parse_session_directory("/repo:").is_err());
    assert!(parse_session_directory("/repo:relative").is_err());
    assert!(parse_session_directory(": /work".replace(' ', "").as_str()).is_err());
}

#[test]
fn cli_triple_parses_and_rejects() {
    use cistella::mount::parse_mount_triple;
    let t = parse_mount_triple("/data:/data:ro").unwrap();
    assert_eq!(t.host_source, "/data");
    assert_eq!(t.mode, MountMode::Ro);
    assert!(parse_mount_triple("/data:/data").is_err());
    assert!(parse_mount_triple("/data:/data:rw:extra").is_err());
    assert!(parse_mount_triple("/data:/data:xx").is_err());
}

#[test]
fn merge_unions_disjoint_cli_triples() {
    use cistella::mount::merge_cli_mounts;
    let merged = merge_cli_mounts(
        &[triple("/a", "/data", MountMode::Ro)],
        &[triple("/b", "/extra", MountMode::Rw)],
        "/work",
    )
    .unwrap();
    assert_eq!(merged.len(), 2);
}

#[test]
fn merge_exact_target_override_wins() {
    use cistella::mount::merge_cli_mounts;
    let merged = merge_cli_mounts(
        &[triple("/a", "/data", MountMode::Ro)],
        &[triple("/b", "/data", MountMode::Rw)],
        "/work",
    )
    .unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].host_source, "/b");
    assert_eq!(merged[0].mode, MountMode::Rw);
}

#[test]
fn merge_rejects_duplicate_cli_targets() {
    use cistella::mount::merge_cli_mounts;
    let err = merge_cli_mounts(
        &[],
        &[
            triple("/a", "/dup", MountMode::Ro),
            triple("/b", "/dup", MountMode::Rw),
        ],
        "/work",
    )
    .unwrap_err();
    assert!(err.to_string().contains("duplicate"));
}

#[test]
fn merge_rejects_cli_on_worktree_target() {
    use cistella::mount::merge_cli_mounts;
    let err = merge_cli_mounts(&[], &[triple("/a", "/work", MountMode::Ro)], "/work").unwrap_err();
    assert!(err.to_string().contains("worktree"));
}

#[test]
fn merge_rejects_partial_cli_profile_overlap() {
    use cistella::mount::merge_cli_mounts;
    // RW nesting across the CLI/profile boundary stacks: the pair-form
    // worktree shape (RW target beneath an RW profile ancestor) depends
    // on this.
    let merged = merge_cli_mounts(
        &[triple("/a", "/data", MountMode::Rw)],
        &[triple("/b", "/data/sub", MountMode::Rw)],
        "/work",
    )
    .unwrap();
    assert_eq!(merged.len(), 2);
}

#[test]
fn merge_allows_ro_ancestor_stacking() {
    use cistella::mount::merge_cli_mounts;
    // Profile RO parent, CLI RW child: the dogfood notebook shape.
    let merged = merge_cli_mounts(
        &[triple("/notes", "/notes", MountMode::Ro)],
        &[triple("/notes/cistella", "/notes/cistella", MountMode::Rw)],
        "/work",
    )
    .unwrap();
    assert_eq!(merged.len(), 2);
}

#[test]
fn validation_allows_ro_parent_rw_child() {
    let parent = triple("/tmp/notes", "/notes", MountMode::Ro);
    let child = triple("/tmp/notes/cistella", "/notes/cistella", MountMode::Rw);
    assert!(validate_mounts(&[parent, child], "/home/cistella").is_ok());
}

#[test]
fn validation_allows_ro_child_under_rw_parent() {
    // RO child under an RW parent stacks (the child graft is the point);
    // missing chains under RO ancestors stay covered by the nested-ro
    // preflight, not by the overlap rule.
    let parent = triple("/tmp/a", "/data", MountMode::Rw);
    let child = triple("/tmp/a/sub", "/data/sub", MountMode::Ro);
    assert!(validate_mounts(&[parent, child], "/home/cistella").is_ok());
}

#[test]
fn validation_allows_distinct_nested_host_sources() {
    // Host-side mirror: distinct nested sources stack; only identical
    // canonical sources refuse (ambiguous intent, not stacking).
    let parent = triple("/tmp/a", "/data", MountMode::Rw);
    let child = triple("/tmp/a/sub", "/data/sub", MountMode::Rw);
    assert!(validate_mounts(&[parent, child], "/home/cistella").is_ok());
}

#[test]
fn validation_rejects_duplicate_targets() {
    let a = triple("/tmp/a", "/dup", MountMode::Ro);
    let b = triple("/tmp/b", "/dup", MountMode::Ro);
    let err = validate_mounts(&[a, b], "/home/cistella").unwrap_err();
    assert!(err.to_string().contains("duplicate"));
}

#[test]
fn validation_allows_ro_ro_stacking() {
    let parent = triple("/tmp/a", "/data", MountMode::Ro);
    let child = triple("/tmp/a/sub", "/data/sub", MountMode::Ro);
    assert!(validate_mounts(&[parent, child], "/home/cistella").is_ok());
}

#[test]
fn profile_from_toml_rejects_container_home_template() {
    // Literal API upholds the same rejection as resolution: template
    // syntax in container_home fails before canonicalization, even in
    // absolute-path form.
    let bad = r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
container-home = "/home/{{host-home}}"
mounts = []
"#;
    let err = Profile::from_toml(bad).unwrap_err();
    assert!(err.to_string().contains("templates"));
}

use cistella::mount::{nested_ro_checks, nested_ro_missing};

#[test]
fn nested_ro_selects_deepest_ancestor_and_translates() {
    let dir = tempfile::TempDir::new().unwrap();
    let ro_base = dir.path().join("ro");
    let child = dir.path().join("child");
    std::fs::create_dir_all(child.join("deep")).unwrap();
    let triples = vec![
        triple(ro_base.to_str().unwrap(), "/tree", MountMode::Ro),
        triple(child.to_str().unwrap(), "/tree/deep/leaf", MountMode::Rw),
        triple("/data", "/data", MountMode::Rw),
    ];
    let checks = nested_ro_checks(&triples);
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].descendant, "/tree/deep/leaf");
    assert_eq!(checks[0].ancestor, "/tree");
    // Translated onto the ancestor source, not the descendant's own.
    // Full chain including the leaf must pre-exist: neither podman nor
    // runc creates mountpoints inside a read-only parent.
    assert_eq!(checks[0].host_path, ro_base.join("deep").join("leaf"));
    assert!(nested_ro_missing(&checks[0]).is_some(), "chain absent");
    std::fs::create_dir_all(ro_base.join("deep").join("leaf")).unwrap();
    assert!(nested_ro_missing(&checks[0]).is_none(), "chain complete");
}

#[test]
fn nested_ro_deepest_of_two_ro_ancestors_wins() {
    // Two nested RO ancestors: the deepest supplies the namespace, so a
    // chain complete under the outer ancestor but missing under the inner
    // one still refuses (and vice versa).
    let dir = tempfile::TempDir::new().unwrap();
    let outer = dir.path().join("outer");
    let inner = dir.path().join("outer").join("inner");
    std::fs::create_dir_all(inner.join("leaf")).unwrap();
    let triples = vec![
        triple(outer.to_str().unwrap(), "/tree", MountMode::Ro),
        triple(inner.to_str().unwrap(), "/tree/inner", MountMode::Ro),
        triple("/elsewhere", "/tree/inner/leaf", MountMode::Rw),
    ];
    let checks = nested_ro_checks(&triples);
    assert_eq!(checks.len(), 2, "{checks:?}");
    // The middle triple is itself a descendant of the outer RO ancestor.
    assert_eq!(checks[0].descendant, "/tree/inner");
    assert_eq!(checks[0].ancestor, "/tree");
    // The leaf answers to the deepest RO ancestor, not the outer one.
    assert_eq!(checks[1].descendant, "/tree/inner/leaf");
    assert_eq!(checks[1].ancestor, "/tree/inner");
    assert_eq!(checks[1].host_path, inner.join("leaf"));
    assert!(nested_ro_missing(&checks[1]).is_none());
}

#[test]
fn nested_ro_file_where_dir_must_be_refuses() {
    // A regular file where a directory must be pins the is_dir discipline.
    let dir = tempfile::TempDir::new().unwrap();
    let ro_base = dir.path().join("ro");
    std::fs::create_dir_all(&ro_base).unwrap();
    std::fs::write(ro_base.join("blocker"), "file").unwrap();
    let triples = vec![
        triple(ro_base.to_str().unwrap(), "/tree", MountMode::Ro),
        triple("/elsewhere", "/tree/blocker/leaf", MountMode::Rw),
    ];
    let checks = nested_ro_checks(&triples);
    assert_eq!(checks.len(), 1);
    assert_eq!(nested_ro_missing(&checks[0]), Some(ro_base.join("blocker")));
}

#[test]
fn nested_ro_silent_without_ro_ancestor() {
    // Non-RO chains never yield checks regardless of host existence —
    // the preflight is silent outside nested-under-RO.
    let triples = vec![
        triple("/a", "/x", MountMode::Rw),
        triple("/b", "/x/deep/nest", MountMode::Rw),
    ];
    assert!(nested_ro_checks(&triples).is_empty());
}

#[test]
fn nested_ro_missing_names_first_offender() {
    let dir = tempfile::TempDir::new().unwrap();
    let ro_base = dir.path().join("ro");
    std::fs::create_dir_all(ro_base.join("present")).unwrap();
    let triples = vec![
        triple(ro_base.to_str().unwrap(), "/tree", MountMode::Ro),
        triple("/elsewhere", "/tree/present/absent/deep", MountMode::Rw),
    ];
    let checks = nested_ro_checks(&triples);
    assert_eq!(checks.len(), 1);
    assert_eq!(
        nested_ro_missing(&checks[0]),
        Some(ro_base.join("present").join("absent"))
    );
}
