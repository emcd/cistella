//! Mountpoint-preparation helpers: canonical bind set and stat parsing.

use std::collections::HashMap;

use cistella::mount::volume_targets;
use cistella::prepare::parse_stat_map;

#[test]
fn volume_targets_parse_and_canonicalize() {
    // Single source of truth: every emitted --volume entry contributes
    // its container side in canonical form, so authorization cannot miss
    // a mount prefix whose profile spelling was noncanonical. Tmpfs
    // entries contribute nothing (home is handled separately).
    let volumes = vec![
        "--tmpfs".to_string(),
        "/home/cistella".to_string(),
        "--volume".to_string(),
        "/data/./x:/home/cistella/.config//deep:rw".to_string(),
        "--volume".to_string(),
        "/run/seat.sock:/run/cistella/seat.sock:ro".to_string(),
    ];
    assert_eq!(
        volume_targets(&volumes),
        vec![
            "/home/cistella/.config/deep".to_string(),
            "/run/cistella/seat.sock".to_string(),
        ]
    );
}

#[test]
fn parse_stat_map_survives_spaces() {
    // `%u %n` puts the uid first so names with spaces parse; missing
    // paths have no entry (the created-vs-preexisting signal).
    let map = parse_stat_map("0 /home/cistella/.config\n1000 /home/cistella/my dir\n");
    assert_eq!(
        map.get("/home/cistella/.config").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        map.get("/home/cistella/my dir").map(String::as_str),
        Some("1000")
    );
    assert_eq!(map.len(), 2);
}

#[test]
fn parse_stat_map_empty() {
    let map: HashMap<String, String> = parse_stat_map("");
    assert!(map.is_empty());
}

use cistella::mount::{MountMode, MountTriple, preparation_sources};

fn triple(host: &str, target: &str, mode: MountMode) -> MountTriple {
    MountTriple {
        host_source: host.to_string(),
        container_target: target.to_string(),
        mode,
    }
}

#[test]
fn preparation_sources_cover_user_mounts_only() {
    // Profile + CLI + session targets seed candidates; credential-style
    // internals never do (they join the authorization set via
    // volume_targets, not here).
    let profile = vec![triple("/data", "/home/cistella/.config/x", MountMode::Rw)];
    let cli = vec![triple("/n", "/srv/data", MountMode::Ro)];
    let got = preparation_sources(&profile, &cli, "/work");
    assert!(
        got.contains(&"/home/cistella/.config/x".to_string()),
        "{got:?}"
    );
    assert!(got.contains(&"/srv/data".to_string()), "{got:?}");
    assert!(got.contains(&"/work".to_string()), "{got:?}");
}

#[test]
fn ssh_expectation_builds_exact_tuple_from_argv() {
    use cistella::framework::prepare::ssh_expectation;
    let extra = vec![
        "--socket=/run/seat.sock".to_string(),
        "--socket-canonical=/run/user/1000/agent.sock".to_string(),
    ];
    let expected = ssh_expectation(&extra).expect("argv parses");
    assert_eq!(
        expected.owner,
        cistella::framework::contract::Provenance::Extension("shipped:ssh".to_string())
    );
    assert_eq!(
        expected.env,
        vec![("SSH_AUTH_SOCK".to_string(), "/run/seat.sock".to_string())]
    );
    assert_eq!(
        expected.mounts,
        vec![(
            "/run/user/1000/agent.sock".to_string(),
            "/run/seat.sock".to_string()
        )]
    );
    assert_eq!(
        expected.permissions,
        vec![(
            "/run/seat.sock".to_string(),
            cistella::framework::contract::Rights::Read
        )]
    );
    assert_eq!(
        expected.credentials,
        vec![(
            "seat-socket".to_string(),
            "/run/user/1000/agent.sock".to_string()
        )]
    );
}

#[test]
fn ssh_expectation_absent_without_both_argv() {
    use cistella::framework::prepare::ssh_expectation;
    assert!(ssh_expectation(&[]).is_none());
    assert!(ssh_expectation(&["--socket=/run/seat.sock".to_string()]).is_none());
    assert!(ssh_expectation(&["--socket-canonical=/run/x".to_string()]).is_none());
}

fn ssh_declared_profile(extra: &str) -> cistella::profile::Profile {
    // One declared `ssh` extension plus optional extra TOML.
    cistella::profile::Profile::from_toml(&format!(
        "image = \"localhost/cistella/opencode:example\"\n\
         mounts = []\n\
         [[extensions]]\n\
         name = \"ssh\"\n\
         {extra}"
    ))
    .expect("profile parses")
}

fn ssh_resolutions(
    prof: &cistella::profile::Profile,
) -> Vec<cistella::profile::ExtensionResolution> {
    prof.resolve_extensions(&cistella::guest_registry::Registry::default())
        .expect("ssh resolves shipped")
}

/// Ambient selection contract, sequential in one test: process
/// env is global to the runner, so cases swap it one at a time
/// instead of racing parallel tests.
#[test]
fn ssh_selection_ambient_contract() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::net::UnixListener;

    use crate::ssh_ambient::AmbientGuard;

    let dir = tempfile::tempdir().expect("tmpdir");
    let sock = dir.path().join("agent.sock");
    UnixListener::bind(&sock).expect("socket binds");
    let prof = ssh_declared_profile("");
    let resolutions = ssh_resolutions(&prof);

    // Happy path: argv carries P and canonical C.
    {
        let _guard = AmbientGuard::set(sock.as_os_str());
        let argv = prof
            .ssh_selection_argv(&resolutions)
            .expect("ambient feeds");
        assert_eq!(argv.len(), 1);
        assert_eq!(
            argv[0],
            vec![
                format!("--socket={}", sock.display()),
                format!("--socket-canonical={}", sock.display()),
            ]
        );
    }
    // Unset ambient refuses (declared-but-unprovidable).
    {
        let _guard = AmbientGuard::unset();
        let error = prof
            .ssh_selection_argv(&resolutions)
            .expect_err("unset ambient refuses")
            .to_string();
        assert!(error.contains("SSH_AUTH_SOCK"), "name-only, got: {error}");
        assert!(
            !error.contains("/run"),
            "no path content leaks, got: {error}"
        );
    }
    // Empty and relative ambient refuse.
    for value in ["", "relative/agent.sock"] {
        let _guard = AmbientGuard::set(std::ffi::OsStr::new(value));
        assert!(
            prof.ssh_selection_argv(&resolutions).is_err(),
            "empty/relative refuses: {value}"
        );
    }
    // Missing path refuses.
    {
        let _guard =
            AmbientGuard::set(std::ffi::OsStr::new("/nonexistent-cistella-ssh/agent.sock"));
        assert!(prof.ssh_selection_argv(&resolutions).is_err());
    }
    // Regular file refuses the kind gate.
    {
        let regular = dir.path().join("not-a-socket");
        std::fs::write(&regular, b"nope").expect("regular file writes");
        let _guard = AmbientGuard::set(regular.as_os_str());
        assert!(prof.ssh_selection_argv(&resolutions).is_err());
    }
    // Non-UTF8 canonical refuses instead of retargeting: P is a
    // UTF-8 symlink onto a raw-byte socket path, with a decoy
    // socket bound at the U+FFFD-substituted string.
    {
        let raw = dir
            .path()
            .join(std::ffi::OsString::from_vec(b"ag\xffnt.sock".to_vec()));
        UnixListener::bind(&raw).expect("raw-byte socket binds");
        let link = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&raw, &link).expect("symlink");
        let _guard = AmbientGuard::set(link.as_os_str());
        let error = prof
            .ssh_selection_argv(&resolutions)
            .expect_err("non-UTF8 canonical refuses, never retargets")
            .to_string();
        assert!(error.contains("UTF-8"), "name-only, got: {error}");
    }
    // Profile assignments naming the owned name refuse — same
    // value and different value alike (no silent winner).
    for assigned in [
        sock.to_string_lossy().into_owned(),
        "/other.sock".to_string(),
    ] {
        let conflicted = ssh_declared_profile(&format!(
            "[environment-assignments]\nSSH_AUTH_SOCK = \"{assigned}\"\n"
        ));
        let resolutions = ssh_resolutions(&conflicted);
        let _guard = AmbientGuard::set(sock.as_os_str());
        let error = conflicted
            .ssh_selection_argv(&resolutions)
            .expect_err("assignment conflict refuses")
            .to_string();
        assert!(error.contains("SSH_AUTH_SOCK"), "got: {error}");
    }
    // Undeclared ssh builds nothing and refuses nothing:
    // ambient alone never injects.
    {
        let plain = cistella::profile::Profile::from_toml(
            "image = \"localhost/cistella/opencode:example\"\nmounts = []\n",
        )
        .expect("plain parses");
        let resolutions = plain
            .resolve_extensions(&cistella::guest_registry::Registry::default())
            .expect("empty resolves");
        let _guard = AmbientGuard::set(sock.as_os_str());
        let argv = plain
            .ssh_selection_argv(&resolutions)
            .expect("no ssh, no argv");
        assert!(argv.is_empty());
    }
}
