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
credential_surface = "none"
mounts = []
container_home = "/home/cistella/../../etc"
"#,
    );
    assert!(prof.is_err(), "traversal container_home must be rejected");
}

#[test]
fn rejects_overlap() {
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
    assert!(validate_mounts(&[a, b], "/home/cistella").is_err());
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
container_home = "/home/cistella"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_requires_image() {
    let bad = r#"
credential_surface = "none"
mounts = []
container_home = "/home/cistella"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_requires_mounts() {
    let bad = r#"
image = "localhost/cistella/opencode:example"
credential_surface = "none"
container_home = "/home/cistella"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_rejects_reserved_label() {
    let bad = r#"
image = "localhost/cistella/opencode:example"
credential_surface = "none"
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
credential_surface = "none"
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
credential_surface = "none"
[[mounts]]
host_source = "~/.config/opencode"
container_target = "/home/cistella/.config/opencode"
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
credential_surface = "none"
mounts = []
container_home = "/home/cistella"
[env]
HOME = "/override"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_accepts_valid() {
    let ok = r#"
image = "localhost/cistella/opencode:example"
credential_surface = "none"
container_home = "/home/cistella"
[[mounts]]
host_source = "/tmp/a"
container_target = "/work"
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
    // WW nesting across the CLI/profile boundary stays fail-closed.
    assert!(
        merge_cli_mounts(
            &[triple("/a", "/data", MountMode::Rw)],
            &[triple("/b", "/data/sub", MountMode::Rw)],
            "/work",
        )
        .is_err()
    );
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
fn validation_rejects_ww_nesting() {
    let parent = triple("/tmp/a", "/data", MountMode::Rw);
    let child = triple("/tmp/a/sub", "/data/sub", MountMode::Ro);
    assert!(validate_mounts(&[parent, child], "/home/cistella").is_err());
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
credential_surface = "none"
container_home = "/home/{{host-home}}"
mounts = []
"#;
    let err = Profile::from_toml(bad).unwrap_err();
    assert!(err.to_string().contains("templates"));
}
