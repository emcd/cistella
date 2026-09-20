//! Label plumbing: JSON round-trip, spaced directories, inspect failure.

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn gc_inspect_failure_fails_closed() {
    // A present-but-uninspectable container is an invocation failure, not
    // Quadlet `--rm` absence: gc must error and reap nothing.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let home = home_dir();
    let shim_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".auxiliary/temporary/podman-shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let real_podman = String::from_utf8_lossy(
        &Command::new("which")
            .arg("podman")
            .output()
            .expect("which podman")
            .stdout,
    )
    .trim()
    .to_string();
    std::fs::write(
        shim_dir.join("podman"),
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = \"inspect\" ]; then\n    echo \"simulated I/O error\" >&2\n    exit 1\n  fi\ndone\nexec \"{real_podman}\" \"$@\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            shim_dir.join("podman"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let (mut conduct, id, mut guard) =
        spawn_conduct(&home, &worktree_str, &["--identity", "alice"]);
    wait_active(&id);
    let container = format!("cistella-{id}");

    let shim_path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(bin())
        .arg("gc")
        .env("HOME", &home)
        .env("TERM", "xterm-ghostty")
        .env("PATH", &shim_path)
        .output()
        .expect("spawn gc");
    assert!(
        !out.status.success(),
        "gc must fail closed on inspect error"
    );
    // Nothing reaped: unit, liveness, and scratch all intact.
    assert!(
        unit_path(&home, &id).exists(),
        "unit untouched by failed gc"
    );
    assert!(!scratch_gone(&id), "scratch untouched by failed gc");
    let out = run_cistella(&home, &["survey"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&container));

    let out = run_cistella(&home, &["terminate", &id]);
    assert!(out.status.success());
    let _ = conduct.wait();
    guard.id = None;
    assert!(scratch_gone(&id));
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn command_label_edge_round_trip_live() {
    // Spaces, quotes, and `=` must survive Quadlet file -> podman label ->
    // JSON argv, and the harness must execute verbatim.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let fixture = fixture_profile("default.toml");
    // `%h`/`%%` must survive as literals: systemd expands bare specifiers
    // at start time, so the driver doubles them in the unit.
    let script = "echo \"a=b c'd 100% %h %%\" > /tmp/edge_probe; sleep 60";
    let argv = ["sh", "-c", script];
    let expected_json = serde_json::to_string(&argv).unwrap();

    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        fixture.as_str(),
        &worktree_str,
        &["--identity", "alice"],
        &argv,
        &[],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");

    // Unit-file value (registry view) equals the JSON argv string.
    assert_eq!(unit_label(&home, &id, "cistella.command"), expected_json);
    // Podman-side value (runtime view) equals the same JSON string.
    assert_eq!(podman_label(&container, "cistella.command"), expected_json);

    // The harness ran verbatim: the probe file has the exact bytes,
    // including literal `%h`/`%%` (no systemd expansion in argv).
    let out = run_cistella(&home, &["enter", &id, "--", "cat", "/tmp/edge_probe"]);
    assert!(
        out.status.success(),
        "enter cat: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "a=b c'd 100% %h %%"
    );

    let out = run_cistella(&home, &["terminate", &id]);
    assert!(out.status.success());
    let _ = conduct.wait();
    guard.id = None;
    assert!(scratch_gone(&id));
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn spaced_directory_conduct() {
    // Host paths with spaces must survive Volume= quoting and directory labels.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let spaced = worktree.path().join("my dir");
    std::fs::create_dir(&spaced).unwrap();
    std::fs::write(spaced.join("README.md"), "# test").unwrap();
    let spaced_str = spaced.to_string_lossy().to_string();
    let home = home_dir();

    let (mut conduct, id, mut guard) = spawn_conduct(&home, &spaced_str, &["--identity", "alice"]);
    wait_active(&id);
    let container = format!("cistella-{id}");
    assert_eq!(unit_label(&home, &id, "cistella.directory"), spaced_str);
    assert_eq!(podman_label(&container, "cistella.directory"), spaced_str);

    let out = run_cistella(&home, &["survey", "--directory", &spaced_str]);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&container));
    // The /work mount resolves through the quoted Volume= triple.
    let out = run_cistella(
        &home,
        &["enter", &id, "--", "test", "-f", "/work/README.md"],
    );
    assert!(
        out.status.success(),
        "work mount with space: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_cistella(&home, &["terminate", &id]);
    assert!(out.status.success());
    let _ = conduct.wait();
    guard.id = None;
    assert!(scratch_gone(&id));

    // A literal `%` in the path must survive systemd specifier expansion
    // via `%%` doubling in both the Volume source and the directory label.
    let pct = worktree.path().join("100%hoff");
    std::fs::create_dir(&pct).unwrap();
    std::fs::write(pct.join("README.md"), "# test").unwrap();
    let pct_str = pct.to_string_lossy().to_string();
    let (mut conduct, id, mut guard) = spawn_conduct(&home, &pct_str, &["--identity", "alice"]);
    wait_active(&id);
    let container = format!("cistella-{id}");
    assert_eq!(unit_label(&home, &id, "cistella.directory"), pct_str);
    assert_eq!(podman_label(&container, "cistella.directory"), pct_str);
    let out = run_cistella(
        &home,
        &["enter", &id, "--", "test", "-f", "/work/README.md"],
    );
    assert!(
        out.status.success(),
        "work mount with percent: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(out.status.success());
    let _ = conduct.wait();
    guard.id = None;
    assert!(scratch_gone(&id));
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn environment_values_verbatim() {
    // Env values with spaces, quotes, `=`, and `%` must arrive verbatim
    // through the quoted `Environment=` directives (no systemd corruption).
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = worktree.path().join("env.toml");
    std::fs::write(
        &profile,
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         container-home = \"/home/cistella\"\n\
         command = [\"sleep\", \"300\"]\n\
         mounts = []\n\
         [environment]\n\
         EDGE = \"a=b c\\\"d'e 100% %h\"\n",
    )
    .unwrap();

    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile.to_string_lossy(),
        &worktree_str,
        &["--identity", "alice"],
        &["sleep", "300"],
        &[],
    );
    wait_active(&id);
    let out = run_cistella(&home, &["enter", &id, "--", "printenv", "EDGE"]);
    assert!(
        out.status.success(),
        "enter printenv: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "a=b c\"d'e 100% %h"
    );
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(out.status.success());
    let _ = conduct.wait();
    guard.id = None;
    assert!(scratch_gone(&id));
}
