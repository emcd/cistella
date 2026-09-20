//! Mountpoint-preparation live coverage: sibling creation, aliasing,
//! RO exclusion, noncanonical spellings, forced timeout.

use std::process::Command;

use tempfile::TempDir;

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn mountpoint_parents_prepared_for_siblings() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let deep_host = TempDir::new().unwrap();
    let deep_str = deep_host.path().to_string_lossy().to_string();

    // Multi-level deep target: podman auto-creates the parents
    // root-owned; preparation must make sibling creation work.
    // (Spike reproduction: pre-change this fails Permission denied.)
    let profile = worktree.path().join("tmpl-deep.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{deep_str}\"\n\
             container-target = \"/home/cistella/.config/deep/nest\"\n\
             mode = \"rw\"\n"
        ),
    )
    .unwrap();
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "mkdir /home/cistella/.config/agentmux && echo SIBLING_OK; \
             [ \"$(stat -c %u /tmp)\" = \"0\" ] && echo TMP_UNTOUCHED; \
             [ \"$(stat -c %u /run)\" = \"0\" ] && echo RUN_UNTOUCHED",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success() && stdout.contains("SIBLING_OK"),
        "sibling creation under prepared parents: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("TMP_UNTOUCHED") && stdout.contains("RUN_UNTOUCHED"),
        "system ancestors never chowned: {stdout}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn mountpoint_host_alias_conducts() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // Host-side aliasing is benign: a triple whose host source traverses
    // a host symlink canonicalizes before validation, and in-container
    // resolution sees no alias (host symlinks do not create container
    // symlinks). The session conducts and sibling creation works.
    // (A container-side alias into a bind cannot be admitted at all:
    // strict nesting stacks, so the runtime refusal path covers only
    // image-baked symlinks — proven by unit tests on the authorization
    // predicate, not a live session.)
    let real = TempDir::new().unwrap();
    std::fs::write(real.path().join("canary"), "host content").unwrap();
    let linkdir = TempDir::new().unwrap();
    std::os::unix::fs::symlink(real.path(), linkdir.path().join("alias")).unwrap();
    let aliased = linkdir.path().join("alias");
    let profile = worktree.path().join("tmpl-adv.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/home/cistella/.config/deep/nest\"\n\
             mode = \"rw\"\n",
            aliased.to_string_lossy(),
        ),
    )
    .unwrap();
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "cat /home/cistella/.config/deep/nest/canary && echo ALIAS_OK; \
             mkdir /home/cistella/.config/agentmux && echo SIBLING_OK",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success() && stdout.contains("ALIAS_OK"),
        "host-aliased source conducts: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("SIBLING_OK"),
        "sibling creation works: {stdout}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn mountpoint_nested_ro_excluded() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // An unrelated RO bind subtree must emerge from preparation byte
    // untouched while tmpfs-nested chains are prepared normally: the
    // session conducts, the RO content is unmodified, and a sibling under
    // the prepared tmpfs chain works. (A nested-under-RO *target* cannot
    // start at all — runc cannot mkdir inside the RO parent — so that
    // topology is covered by unit exclusion tests, not a live session.)
    let tree = TempDir::new().unwrap();
    std::fs::write(tree.path().join("canary"), "host content").unwrap();
    let deep_host = TempDir::new().unwrap();
    let profile = worktree.path().join("tmpl-nro.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/tree\"\n\
             mode = \"ro\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/home/cistella/.config/deep/nest\"\n\
             mode = \"rw\"\n",
            tree.path().to_string_lossy(),
            deep_host.path().to_string_lossy(),
        ),
    )
    .unwrap();
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "cat /tree/canary && echo RO_INTACT; \
             mkdir /home/cistella/.config/agentmux && echo SIBLING_OK",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success() && stdout.contains("RO_INTACT"),
        "RO subtree readable and unmodified: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("SIBLING_OK"),
        "tmpfs-nested sibling creation works: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(tree.path().join("canary")).unwrap(),
        "host content",
        "host side unmutated"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn mountpoint_noncanonical_spelling_prepared() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let deep_host = TempDir::new().unwrap();
    let deep_str = deep_host.path().to_string_lossy().to_string();

    // Noncanonical profile spelling (`/./` and `//`): the bind set is
    // built from emitted canonical targets, so authorization still
    // matches the real mount prefix and preparation works.
    let profile = worktree.path().join("tmpl-nc.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{deep_str}\"\n\
             container-target = \"/home/cistella/./.config//deep/nest\"\n\
             mode = \"rw\"\n"
        ),
    )
    .unwrap();
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "mkdir /home/cistella/.config/agentmux && echo SIBLING_OK",
        ],
    );
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("SIBLING_OK"),
        "noncanonical target prepared: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn mountpoint_prepare_timeout_fails_closed() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let deep_host = TempDir::new().unwrap();
    let deep_str = deep_host.path().to_string_lossy().to_string();

    // A deep target guarantees preparation candidates exist (the default
    // profile's triples yield none, which correctly no-ops before any
    // exec). A 1 ms preparation deadline fires on the first exec.
    let profile = worktree.path().join("tmpl-timeout.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{deep_str}\"\n\
             container-target = \"/home/cistella/.config/deep/nest\"\n\
             mode = \"rw\"\n"
        ),
    )
    .unwrap();
    let out = Command::new(bin())
        .args([
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "true",
        ])
        .env("HOME", &home)
        .env("TERM", "xterm-ghostty")
        .env("CISTELLA_PREPARE_EXEC_TIMEOUT_MS", "1")
        .output()
        .expect("spawn cistella");
    assert!(
        !out.status.success(),
        "forced preparation timeout must fail conduct"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("deadline"),
        "typed deadline error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Residue check: no unit files or scratches remain for the attempt.
    // The id is unknown (conduct never printed it), so scan the registry
    // for leftovers from this worktree instead.
    let survey = run_cistella(&home, &["survey", "--directory", &worktree_str]);
    assert!(
        !String::from_utf8_lossy(&survey.stdout).contains("cistella-"),
        "no residue sessions: {}",
        String::from_utf8_lossy(&survey.stdout)
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn nested_ro_preexisting_chain_succeeds() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // Full intermediate chain pre-exists in the RO source: preflight
    // passes, child is writable, parent stays RO.
    let tree = TempDir::new().unwrap();
    let leaf_host = TempDir::new().unwrap();
    // Full chain including the leaf pre-exists in the RO source: neither
    // podman nor runc creates mountpoints inside a read-only parent.
    std::fs::create_dir_all(tree.path().join("deep").join("leaf")).unwrap();
    let profile = worktree.path().join("tmpl-pre.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/tree\"\n\
             mode = \"ro\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/tree/deep/leaf\"\n\
             mode = \"rw\"\n",
            tree.path().to_string_lossy(),
            leaf_host.path().to_string_lossy(),
        ),
    )
    .unwrap();
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "touch /tree/deep/leaf/ok && echo LEAF_OK; \
             touch /tree/parent-write 2>/dev/null && echo PARENT_UNEXPECTED || echo PARENT_RO",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success() && stdout.contains("LEAF_OK"),
        "preexisting nested chain conducts: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("PARENT_RO"), "parent stays RO: {stdout}");
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn nested_ro_missing_chain_refuses_pre_mutation() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // Intermediate link absent in the RO source: conduct refuses with a
    // typed error naming both sides, before unit file or scratch exist.
    let tree = TempDir::new().unwrap();
    let leaf_host = TempDir::new().unwrap();
    let profile = worktree.path().join("tmpl-miss.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/tree\"\n\
             mode = \"ro\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/tree/deep/leaf\"\n\
             mode = \"rw\"\n",
            tree.path().to_string_lossy(),
            leaf_host.path().to_string_lossy(),
        ),
    )
    .unwrap();
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "true",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "missing chain must refuse");
    assert!(
        stderr.contains("/tree") && stderr.contains("read-only"),
        "typed error names ancestor: {stderr}"
    );
    // No-residue proof is directory-scoped, not registry-wide: the
    // refused conduct never mints an id, so survey for this worktree must
    // show no session — immune to concurrent tests' unit files.
    let survey = run_cistella(&home, &["survey", "--directory", &worktree_str]);
    assert!(
        !String::from_utf8_lossy(&survey.stdout).contains("cistella-"),
        "no residue session for refused conduct: {}",
        String::from_utf8_lossy(&survey.stdout)
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn nested_ro_session_dogfood_shape_passes() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    // The dogfood shape: session-directory target nested under an RO
    // profile triple, intermediates pre-existing on the host. Preflight
    // passes and the session conducts.
    let ro_base = TempDir::new().unwrap();
    std::fs::create_dir_all(ro_base.path().join("proj/sub")).unwrap();
    let profile = ro_base.path().join("tmpl-dog.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/base\"\n\
             mode = \"ro\"\n",
            ro_base.path().to_string_lossy(),
        ),
    )
    .unwrap();
    let home = home_dir();
    let session = ro_base.path().join("proj/sub");
    let session_str = session.to_string_lossy().to_string();
    let pair = format!("{session_str}:/base/proj/sub");
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &pair,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "pwd && echo DOGFOOD_OK",
        ],
    );
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("DOGFOOD_OK"),
        "dogfood shape conducts: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn nested_ro_symlinked_source_resolves() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    // Ancestor host source behind a symlink: canonicalization supplies
    // the resolved namespace and the preexisting chain passes.
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let real = TempDir::new().unwrap();
    std::fs::create_dir_all(real.path().join("deep").join("leaf")).unwrap();
    let linkdir = TempDir::new().unwrap();
    std::os::unix::fs::symlink(real.path(), linkdir.path().join("alias")).unwrap();
    let leaf_host = TempDir::new().unwrap();
    let profile = worktree.path().join("tmpl-sym.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{}/alias\"\n\
             container-target = \"/tree\"\n\
             mode = \"ro\"\n\
             [[mounts]]\n\
             host-source = \"{}\"\n\
             container-target = \"/tree/deep/leaf\"\n\
             mode = \"rw\"\n",
            linkdir.path().to_string_lossy(),
            leaf_host.path().to_string_lossy(),
        ),
    )
    .unwrap();
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "touch /tree/deep/leaf/ok && echo SYMLINK_OK",
        ],
    );
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("SYMLINK_OK"),
        "symlinked source resolves: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn nested_rw_under_rw_conducts() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // The seat-restart shape: a wholesale RW profile ancestor with the
    // pair-form worktree nested beneath it. RW-under-RW stacks (podman
    // mounts parent-first, mkdir works through RW parents); only exact
    // duplicates refuse.
    let parent = TempDir::new().unwrap();
    let parent_str = parent.path().to_string_lossy().to_string();
    let profile = worktree.path().join("tmpl-nested-rw.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [[mounts]]\n\
             host-source = \"{parent_str}\"\n\
             container-target = \"/ns-parent\"\n\
             mode = \"rw\"\n",
        ),
    )
    .unwrap();
    let pair = format!("{worktree_str}:/ns-parent/child");
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &pair,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "echo data > /ns-parent/child/from-child && cat /ns-parent/child/from-child",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success() && stdout.contains("data"),
        "nested RW worktree conducts: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // The write landed on the host worktree through the nested mount.
    assert!(
        worktree.path().join("from-child").exists(),
        "nested write reaches host worktree"
    );
}
