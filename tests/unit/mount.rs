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
harness = "opencode"
credential_surface = "none"
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
harness = "opencode"
container_home = "/home/cistella"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_rejects_home_in_env() {
    let bad = r#"
harness = "opencode"
credential_surface = "none"
container_home = "/home/cistella"
[env]
HOME = "/override"
"#;
    assert!(Profile::from_toml(bad).is_err());
}

#[test]
fn profile_accepts_valid() {
    let ok = r#"
harness = "opencode"
credential_surface = "none"
container_home = "/home/cistella"
[[mounts]]
host_source = "/tmp/a"
container_target = "/work"
mode = "rw"
"#;
    assert!(Profile::from_toml(ok).is_ok());
}
