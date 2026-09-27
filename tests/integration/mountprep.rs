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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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
    let worktree = src_worktree();
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

/// Same-tree RW graft refuses pre-create through real conduct:
/// a read-write graft of ancestor-tree content outside the
/// session subtree would dentry-alias RO-covered content
/// (Landlock unions aliases toward FULL), so the hooked branch
/// refuses with the graft named before any unit or scratch.
/// The graft target sits outside all routes (no RO ancestor
/// above it) so nested_ro stays silent and the refusal proves
/// the alias gate specifically.
#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn same_tree_rw_graft_refuses_pre_create() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // Sibling of the session dir: strictly under ~/src,
    // outside the session subtree (random name, auto-cleaned).
    let sib = tempfile::TempDir::new_in(worktree.path().parent().expect("worktree sits in ~/src"))
        .expect("sibling tempdir");
    let sib_str = sib.path().to_string_lossy().to_string();
    let src_root = format!("{home}/src");
    let profile = worktree.path().join("tmpl-graft-alias.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [isolator]\n\
             name = \"podman\"\n\
             [[extensions]]\n\
             name = \"landlock\"\n\
             [[mounts]]\n\
             host-source = \"{src_root}\"\n\
             container-target = \"/src\"\n\
             mode = \"ro\"\n\
             [[mounts]]\n\
             host-source = \"{sib_str}\"\n\
             container-target = \"/graft\"\n\
             mode = \"rw\"\n",
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
    assert!(!out.status.success(), "aliasing graft must refuse");
    assert!(
        stderr.contains("/graft") && stderr.contains("aliases read-only ancestor content"),
        "typed error names the graft: {stderr}"
    );
    // Refused conduct mints no session: survey stays clean.
    let survey = run_cistella(&home, &["survey", "--directory", &worktree_str]);
    assert!(
        !String::from_utf8_lossy(&survey.stdout).contains("cistella-"),
        "no residue session for refused conduct: {}",
        String::from_utf8_lossy(&survey.stdout)
    );
}

/// Hooked conduct happy path through real conduct: a declared
/// Landlock session confines end-to-end (create, stage,
/// initiate, hooked launch, teardown). The harness proves
/// hookedness intrinsically: the staged wrapper path exists
/// ONLY in hooked sessions (staging mounts it per-session),
/// so the probe gate passes if and only if this session
/// confined — a plain session would skip the branch and print
/// nothing. The admitted marker proves the harness ran; the
/// DENIED line proves the ancestor rule denied.
#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn hooked_conduct_confines_declared_session() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let src_root = format!("{home}/src");
    let profile = worktree.path().join("tmpl-hooked-happy.toml");
    std::fs::write(
        &profile,
        format!(
            "image = \"localhost/cistella/opencode:example\"\n\
             credential-surface = \"none\"\n\
             container-home = \"/home/cistella\"\n\
             [isolator]\n\
             name = \"podman\"\n\
             [[extensions]]\n\
             name = \"landlock\"\n\
             [[mounts]]\n\
             host-source = \"{src_root}\"\n\
             container-target = \"/src\"\n\
             mode = \"ro\"\n",
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
            "echo ok > marker; if /run/cistella/hooks/landlock-wrap --probe; then echo bad > /src/escape || echo DENIED; fi",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "hooked conduct must exit 0: {stderr}");
    assert!(
        stdout.contains("DENIED"),
        "ancestor rule must deny through the hooked session: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.path().join("marker"))
            .expect("admitted marker readable")
            .as_str(),
        "ok\n"
    );
}

/// Live adversarial-bind refusal (3.4 part 2): a REAL bind
/// mount shares dentries across spellings that canonicalize
/// apart, so only the dev+ino leg can see it. Both legs run
/// inside one private mount namespace (`podman unshare` +
/// `mount --bind`: unprivileged-capable per QA seat probe,
/// evaporates with the namespace — zero host mutation), plus
/// an undisjointed control that must stay allowed. Skips
/// quietly where the capability is absent.
#[ignore = "live: requires bind-mount capability (podman unshare)"]
#[test]
fn bind_alias_graft_refuses_precreate() {
    let unshare = Command::new("podman")
        .arg("unshare")
        .arg("true")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if !unshare {
        eprintln!("skip: podman unshare unavailable");
        return;
    }
    // Fake HOME confinement tree: ancestor $HOME/src with a
    // project subtree and a denied sibling.
    let home = TempDir::new().expect("fake home");
    let src = home.path().join("src");
    let proj = src.join("proj");
    let sib = src.join("sib");
    std::fs::create_dir_all(&proj).expect("proj dir");
    std::fs::create_dir_all(&sib).expect("sib dir");
    std::fs::write(sib.join("seed"), "seed").expect("sib seed");
    // Disjoint mountpoints (empty dirs on the host) plus a
    // disjoint control graft source with disjoint dentries.
    let alias = TempDir::new().expect("alias mountpoint");
    let tree_bind = TempDir::new().expect("tree bind mountpoint");
    let control_src = TempDir::new().expect("control graft source");
    let probe = example_binary("bind_alias_probe");
    let home_str = home.path().to_string_lossy().to_string();
    let proj_str = proj.to_string_lossy().to_string();
    let sib_str = sib.to_string_lossy().to_string();
    let alias_str = alias.path().to_string_lossy().to_string();
    let src_str = src.to_string_lossy().to_string();
    let tree_bind_str = tree_bind.path().to_string_lossy().to_string();
    let control_str = control_src.path().to_string_lossy().to_string();
    let probe_str = probe.to_string_lossy().to_string();
    // All three cases in one namespace: subdir bind (sib at
    // a disjoint path — mountinfo containment leg), exact
    // bind (ancestor at a disjoint path — dev+ino equality
    // leg), and undisjointed control (must stay allowed).
    // `;`-chained so a mount failure surfaces as a loud
    // product assertion, never a silent pass. Identity lines
    // prove the bind is real (alias shares the sibling
    // dentry) and that the subdir case is NOT an exact-ID
    // match (sibling ino differs from the ancestor ino) —
    // only fs-relative containment can refuse it.
    let script = format!(
        "mount --bind '{sib_str}' '{alias_str}' || echo MOUNT_FAILED_SUBDIR; \
         mount --bind '{src_str}' '{tree_bind_str}' || echo MOUNT_FAILED_EXACT; \
         echo -n 'ids-ancestor:'; stat -c '%d %i' '{src_str}'; \
         echo -n 'ids-sib:'; stat -c '%d %i' '{sib_str}'; \
         echo -n 'ids-alias:'; stat -c '%d %i' '{alias_str}'; \
         echo -n 'case-subdir:'; '{probe_str}' '{home_str}' '{proj_str}' '{alias_str}' /src/graft; \
         echo -n 'case-exact:'; '{probe_str}' '{home_str}' '{proj_str}' '{tree_bind_str}' /src/graft; \
         echo -n 'case-control:'; '{probe_str}' '{home_str}' '{proj_str}' '{control_str}' /src/graft"
    );
    let out = Command::new("podman")
        .arg("unshare")
        .arg("env")
        .arg(format!("HOME={home_str}"))
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("podman unshare must spawn");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "unshare script must run: {stderr}");
    assert!(
        !stdout.contains("MOUNT_FAILED"),
        "bind mounts must establish: {stdout} {stderr}"
    );
    let line = |label: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("missing {label} in:\n{stdout}"))
            .to_string()
    };
    // The bind is real (alias shares the sibling dentry)
    // and the subdir case is not an exact-ID match (only
    // fs-relative containment can refuse it).
    let ids = |label: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("missing {label} in:\n{stdout}"))
            .trim_start_matches(label)
            .to_string()
    };
    let ancestor_ids = ids("ids-ancestor:");
    let sib_ids = ids("ids-sib:");
    let alias_ids = ids("ids-alias:");
    assert_eq!(
        alias_ids, sib_ids,
        "alias bind must share the sibling dentry, got alias={alias_ids} sib={sib_ids}"
    );
    assert_ne!(
        sib_ids, ancestor_ids,
        "subdir case must differ from the ancestor dentry (else exact-ID equality decides, not containment)"
    );
    // Alias-specific diagnostic: a table-noncoverage or
    // device-disagreement refusal (fail-closed) must NOT
    // satisfy these — only the containment/equality legs
    // proving shared dentries with read-only content.
    let alias_diagnostic = "shares dentries with read-only content through a bind mount";
    // Subdir bind: same dentries as the denied sibling under
    // a disjoint spelling — the graft FULL would admit
    // through every alias.
    let subdir = line("case-subdir:");
    assert!(
        subdir.starts_with("case-subdir:REFUSED:")
            && subdir.contains("/src/graft")
            && subdir.contains(alias_diagnostic),
        "subdir bind alias must refuse with the alias diagnostic naming the graft, got: {subdir}"
    );
    // Exact bind: the ancestor itself under another path —
    // dev+ino equality, no table needed.
    let exact = line("case-exact:");
    assert!(
        exact.starts_with("case-exact:REFUSED:")
            && exact.contains("/src/graft")
            && exact.contains(alias_diagnostic),
        "exact bind alias must refuse with the alias diagnostic naming the graft, got: {exact}"
    );
    // Control: disjoint dentries stay allowed.
    assert_eq!(
        line("case-control:"),
        "case-control:ALLOWED",
        "disjoint graft must stay allowed"
    );
    // Execution record: a pass means the binds established
    // and all three cases ran (the skip path returns early;
    // MOUNT_FAILED markers fail loudly above).
    eprintln!("bind-alias cases EXECUTED: subdir+exact refused, control allowed");
}
