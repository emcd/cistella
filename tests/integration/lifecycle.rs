//! Core conduct lifecycle: mint, attached terminate, orphan reap.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::helpers::*;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_exit_passthrough_and_mint() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let fixture = fixture_profile("default.toml");
    let _guard = Guard::empty();

    // Harness exit 42 passes through conduct.
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            fixture.as_str(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sh",
            "-c",
            "exit 42",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let id1 = first
        .strip_prefix("conduct ")
        .expect("conduct id line")
        .to_string();
    assert!(valid_minted_id(&id1));
    assert!(scratch_gone(&id1), "no scratch residue");
    assert!(!unit_path(&home, &id1).exists(), "no unit residue");

    // Harness true exits 0 with a larger, time-sortable id.
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            fixture.as_str(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "true",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let id2 = first
        .strip_prefix("conduct ")
        .expect("conduct id line")
        .to_string();
    assert!(valid_minted_id(&id2));
    assert!(id2 > id1, "minted ids time-sortable: {id1} < {id2}");

    // Caller-supplied ids are rejected (no --session-id flag exists).
    let out = run_cistella(&home, &["conduct", "--session-id", "foo"]);
    assert!(!out.status.success(), "--session-id must not exist");

    // Missing images are typed refusals naming the image (never pulled).
    let profile = worktree.path().join("missing.toml");
    std::fs::write(
        &profile,
        "image = \"localhost/cistella/missing:example\"\ncredential-surface = \"none\"\n",
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
            "--",
            "true",
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("localhost/cistella/missing:example"),
        "refusal names the image"
    );

    // Failure after install (bad harness path is post-start) leaves no residue.
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            fixture.as_str(),
            "--session-directory",
            &worktree_str,
            "--",
            "/nonexistent-harness-binary-xyz",
        ],
    );
    assert!(!out.status.success());
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    if let Some(id) = first.strip_prefix("conduct ") {
        assert!(!unit_path(&home, id).exists(), "no unit residue");
        assert!(scratch_gone(id), "no scratch residue");
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_lifecycle_terminate_while_attached() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let corr = format!("s-{}-{}", std::process::id(), {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000
    });

    let (mut conduct, id, mut guard) = spawn_conduct(
        &home,
        &worktree_str,
        &[
            "--identity",
            "alice",
            "--label",
            &format!("agentmux.session={corr}"),
        ],
    );
    wait_active(&id);

    let container = format!("cistella-{id}");
    let unit = unit_path(&home, &id);
    assert!(unit.exists(), "unit missing after conduct start");
    let content = std::fs::read_to_string(&unit).unwrap();
    assert!(content.contains("Tmpfs=/home/cistella"));
    assert!(content.contains("SuccessExitStatus=143"));
    assert!(!content.contains("Environment=TERM="));
    // cistella.command JSON array round-trips argv -> label -> argv.
    let round = cistella::session::parse_command_label(&unit_label(&home, &id, "cistella.command"))
        .unwrap();
    assert_eq!(round, vec!["sleep".to_string(), "300".to_string()]);
    assert_eq!(unit_label(&home, &id, "agentmux.session"), corr);

    // survey with zero filters and with directory/label filters.
    for args in [
        vec!["survey"],
        vec!["survey", "--directory", &worktree_str],
        vec!["survey", "--label", &format!("agentmux.session={corr}")],
    ] {
        let out = run_cistella(&home, &args);
        assert!(out.status.success());
        let txt = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            txt.contains(&container),
            "survey {args:?} shows session: {txt}"
        );
    }

    // enter via PTY proves driver transport wiring (stty 30 100, not 0 0).
    assert!(
        enter_stty_via_pty(&home, &worktree_str),
        "cistella enter stty via pty failed"
    );

    // terminate from another pane while conduct is attached converges.
    // Full id: concurrent tests mint same-millisecond prefixes, so short
    // prefixes are ambiguous across the shared registry by design.
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = conduct.wait().expect("conduct reaped");
    eprintln!("conduct end: {status:?}");
    guard.id = None;

    assert!(!unit.exists(), "unit file removed by shared teardown");
    assert!(scratch_gone(&id), "scratch removed by shared teardown");
    let show = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "ActiveState",
            "-p",
            "LoadState",
            &format!("{container}.service"),
        ])
        .output()
        .unwrap();
    let show_txt = String::from_utf8_lossy(&show.stdout).to_string();
    assert!(
        show_txt.contains("ActiveState=inactive") || show_txt.contains("LoadState=not-found"),
        "service inactive/not-found: {show_txt}"
    );
    let failed = Command::new("systemctl")
        .args(["--user", "--failed", "--no-legend"])
        .output()
        .unwrap();
    let failed_txt = String::from_utf8_lossy(&failed.stdout).to_string();
    assert!(
        !failed_txt.contains(&container),
        "not in --failed: {failed_txt}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn inspect_postmortem_and_gc_reap_orphan() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();

    let (mut conduct, id, mut guard) = spawn_conduct(&home, &worktree_str, &["--identity", "bob"]);
    wait_active(&id);
    let container = format!("cistella-{id}");

    // SIGKILL conduct: no teardown runs, unit file and scratch remain.
    conduct.kill().expect("kill conduct");
    let _ = conduct.wait();
    guard.id = None;
    std::thread::sleep(Duration::from_millis(500));
    assert!(unit_path(&home, &id).exists(), "orphan unit remains");

    // External stop simulating a crash: container gone (--rm), unit orphaned.
    let out = Command::new("systemctl")
        .args(["--user", "stop", &format!("{container}.service")])
        .output()
        .unwrap();
    assert!(out.status.success());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        unit_path(&home, &id).exists(),
        "unit file outlives external stop"
    );
    assert!(!scratch_gone(&id), "scratch outlives external stop");

    // inspect reads labels plus journald post-mortem from the orphan unit.
    let out = run_cistella(&home, &["inspect", &id]);
    assert!(
        out.status.success(),
        "inspect: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(txt.contains(&container), "inspect names session: {txt}");

    // gc reaps the orphaned unit and scratch without manual pre-cleanup.
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!unit_path(&home, &id).exists(), "gc removes orphan unit");
    assert!(scratch_gone(&id), "gc removes orphan scratch");
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn name_resolves_from_foreign_cwd_live() {
    // A profile name resolves without a source checkout: the child runs
    // from a directory containing no `data/profiles`.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let foreign = TempDir::new().unwrap();
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let fixture = fixture_profile("default.toml");
    let mut cmd = Command::new(bin());
    cmd.args([
        "conduct",
        "--profile",
        fixture.as_str(),
        "--session-directory",
        &worktree_str,
        "--",
        "sleep",
        "300",
    ]);
    cmd.env("HOME", &home);
    cmd.env("TERM", "xterm-ghostty");
    cmd.current_dir(foreign.path());
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut conduct = cmd.spawn().expect("spawn conduct");
    let stdout = conduct.stdout.take().expect("piped stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let id = loop {
        use std::io::BufRead as _;
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for id line"
            );
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if let Some(id) = line.trim().strip_prefix("conduct ") {
            assert!(valid_minted_id(id), "minted id shape: {id}");
            break id.to_string();
        }
    };
    let mut guard = Guard {
        id: Some(id.clone()),
    };
    wait_active(&id);
    assert_eq!(unit_label(&home, &id, "cistella.profile"), "default");
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(out.status.success());
    let _ = conduct.wait();
    guard.id = None;
    assert!(scratch_gone(&id));
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn configuration_directory_plumbing_live() {
    // `--configuration-directory` reaches resolution: a name found only in
    // the supplied dir conducts with that profile.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let config = TempDir::new().unwrap();
    let profiles = config.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("custom.toml"),
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         command = [\"sleep\", \"infinity\"]\n\
         mounts = []\n",
    )
    .unwrap();
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let config_str = config.path().to_string_lossy().to_string();
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        "custom",
        &worktree_str,
        &["--configuration-directory", &config_str],
        &["sleep", "300"],
        &[],
    );
    wait_active(&id);
    assert_eq!(unit_label(&home, &id, "cistella.profile"), "custom");
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(out.status.success());
    let _ = conduct.wait();
    guard.id = None;
    assert!(scratch_gone(&id));
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn harness_runs_in_worktree_target_live() {
    // The session directory is a working directory, not just a mount:
    // the harness starts with cwd at the container target.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let fixture = fixture_profile("default.toml");
    let pair = format!("{worktree_str}:{worktree_str}");
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            fixture.as_str(),
            "--session-directory",
            &pair,
            "--",
            "pwd",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    assert!(
        lines.first().is_some_and(|l| l.starts_with("conduct ")),
        "id line first: {lines:?}"
    );
    assert_eq!(lines.get(1), Some(&worktree_str), "harness cwd: {lines:?}");
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn project_name_template_live() {
    // Templates resolve end to end: explicit flag and directory-basename
    // default both land the mount at the expanded target.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let config = TempDir::new().unwrap();
    let profiles = config.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("tmpl.toml"),
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         command = [\"sleep\", \"infinity\"]\n\
         [[mounts]]\n\
         host-source = \"/tmp\"\n\
         container-target = \"/tmpl-{{core:project-name}}\"\n\
         mode = \"ro\"\n",
    )
    .unwrap();
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let config_str = config.path().to_string_lossy().to_string();
    // Harness target is fixed text; the flag selects which project fills it.
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            "tmpl",
            "--configuration-directory",
            &config_str,
            "--session-directory",
            &worktree_str,
            "--project-name",
            "QAPROJECT",
            "--",
            "ls",
            "-d",
            "/tmpl-QAPROJECT",
        ],
    );
    assert!(
        out.status.success(),
        "explicit project: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("/tmpl-QAPROJECT"),
        "mount landed at expanded target"
    );
    // Basename default: tempdir file name fills the template.
    let basename = worktree
        .path()
        .file_name()
        .expect("tempdir basename")
        .to_string_lossy()
        .to_string();
    let target = format!("/tmpl-{basename}");
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            "tmpl",
            "--configuration-directory",
            &config_str,
            "--session-directory",
            &worktree_str,
            "--",
            "ls",
            "-d",
            &target,
        ],
    );
    assert!(
        out.status.success(),
        "basename default: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&target),
        "mount landed at basename target"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn terminate_stops_promptly_without_sigkill() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();

    let (mut conduct, id, mut guard) = spawn_conduct(&home, &worktree_str, &[]);
    wait_active(&id);
    let service = format!("cistella-{id}.service");

    // Init-forwarded SIGTERM stops the container in ~1 s; without init
    // podman waits out the full 10 s StopTimeout before SIGKILL. Stop is
    // issued here rather than via `terminate` so the service result can be
    // read machine-readable before teardown removes the unit.
    let start = Instant::now();
    let stop = Command::new("systemctl")
        .args(["--user", "stop", &service])
        .output()
        .expect("systemctl stop");
    let elapsed = start.elapsed();
    assert!(
        stop.status.success(),
        "stop: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "stop took {elapsed:?}: SIGTERM was not forwarded (--init missing?)"
    );
    let show = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "ActiveState",
            "-p",
            "Result",
            "-p",
            "ExecMainCode",
            "-p",
            "ExecMainStatus",
            &service,
        ])
        .output()
        .expect("systemctl show");
    let show_txt = String::from_utf8_lossy(&show.stdout).to_string();
    assert!(
        show_txt.contains("ActiveState=inactive"),
        "service inactive: {show_txt}"
    );
    assert!(
        show_txt.contains("Result=success"),
        "clean result, not exit-code: {show_txt}"
    );
    assert!(
        show_txt.contains("ExecMainCode=1"),
        "main process exited (1), not signaled (2): {show_txt}"
    );
    assert!(
        show_txt.contains("ExecMainStatus=143"),
        "sleep exited 143 on forwarded SIGTERM, not 137: {show_txt}"
    );

    // The harness exec ends with the stopped container, so the attached
    // conduct runs shared teardown itself: unit file and scratch converge
    // away with no separate terminate needed.
    let status = conduct.wait().expect("conduct reaped");
    eprintln!("conduct end: {status:?}");
    guard.id = None;
    assert!(!unit_path(&home, &id).exists(), "no unit residue");
    assert!(scratch_gone(&id), "no scratch residue");
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn template_env_and_labels_resolve_live() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let home_var = std::env::var("HOME").expect("HOME set");

    // Profile bearing templates in env values and labels values. The
    // harness prints its environment: this is the assertion the original
    // template verification skipped (it checked mounts only).
    let profile = worktree.path().join("tmpl-env.toml");
    std::fs::write(
        &profile,
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         container-home = \"/home/cistella\"\n\
         mounts = []\n\
         [environment-assignments]\n\
         PROBE_ALL = \"{{core:container-home}}/.config:{{core:host-home}}/.x:{{core:project-name}}\"\n\
         [labels]\n\
         \"tmpl.tag\" = \"{{core:project-name}}-{{core:container-home}}\"\n",
    )
    .unwrap();
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile.to_string_lossy(),
        &worktree_str,
        &["--identity", "alice", "--project-name", "liveproj"],
        &["sleep", "300"],
        &[],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");

    // Harness-observed environment shows substituted values.
    let env_out = Command::new("podman")
        .args(["exec", &container, "sh", "-c", "echo $PROBE_ALL"])
        .output()
        .expect("podman exec env");
    assert!(env_out.status.success(), "podman exec env");
    let observed = String::from_utf8_lossy(&env_out.stdout).trim().to_string();
    assert_eq!(
        observed,
        format!("/home/cistella/.config:{home_var}/.x:liveproj"),
        "env templates resolved in-container"
    );
    // No literal span text survives anywhere in the environment.
    let all_out = Command::new("podman")
        .args(["exec", &container, "sh", "-c", "env"])
        .output()
        .expect("podman exec env dump");
    let all_txt = String::from_utf8_lossy(&all_out.stdout).to_string();
    assert!(
        !all_txt.contains("{{"),
        "no literal spans in environment: {all_txt}"
    );

    // Template label value round-trips through the running container.
    assert_eq!(
        podman_label(&container, "tmpl.tag"),
        "liveproj-/home/cistella",
        "label templates resolved on container"
    );

    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = conduct.wait();
    guard.id = None;
    assert!(!unit_path(&home, &id).exists(), "no unit residue");
    assert!(scratch_gone(&id), "no scratch residue");
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn environment_acceptances_forward_live() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();

    // Relay-style invoker var with transport-hostile content: spaces,
    // `=`, `%`, quotes, backslash, and template-looking text. Set in the
    // test process (unique name, no parallel-test interaction), restored
    // afterwards. Gate-legal by construction (no line breaks).
    let live_value = "sp ace=a%b'c\"d\\e{{supplement:x}}@end";
    let prev = std::env::var("CISTELLA_LIVE_ACCEPT").ok();
    unsafe { std::env::set_var("CISTELLA_LIVE_ACCEPT", live_value) };
    let profile = worktree.path().join("accept-env.toml");
    std::fs::write(
        &profile,
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         container-home = \"/home/cistella\"\n\
         mounts = []\n\
         environment-acceptances = [\"CISTELLA_LIVE_ACCEPT\"]\n",
    )
    .unwrap();
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile.to_string_lossy(),
        &worktree_str,
        &["--identity", "alice", "--project-name", "liveaccept"],
        &["sleep", "300"],
        &[],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");

    // Accepted value visible byte-exact in container env. `printenv`
    // (not shell `echo`) proves exact transport: no rescan, `=`
    // preservation, and actual Quadlet/systemd delivery.
    let env_out = Command::new("podman")
        .args(["exec", &container, "printenv", "CISTELLA_LIVE_ACCEPT"])
        .output()
        .expect("podman exec printenv");
    assert!(env_out.status.success(), "podman exec printenv");
    assert_eq!(
        String::from_utf8_lossy(&env_out.stdout).to_string(),
        format!("{live_value}\n"),
        "accepted invoker env forwarded byte-exact in-container"
    );

    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = conduct.wait();
    guard.id = None;
    match prev {
        Some(v) => unsafe { std::env::set_var("CISTELLA_LIVE_ACCEPT", v) },
        None => unsafe { std::env::remove_var("CISTELLA_LIVE_ACCEPT") },
    }
    assert!(!unit_path(&home, &id).exists(), "no unit residue");
    assert!(scratch_gone(&id), "no scratch residue");
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn environment_acceptances_absent_refuses_residue_free() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();

    // Serialize against concurrent live conducts: while the
    // creation-window lock is held here, no other conduct can create
    // units, scratch, or containers, so the before/after diff is exact.
    // The refusal path never reaches lock acquisition, so no self-deadlock.
    let _creation = cistella::lock::LockGuard::acquire().expect("creation lock");
    let units_before = cistella_unit_names(&home);
    let scratch_before = cistella_scratch_names();

    let prev = std::env::var("CISTELLA_LIVE_REFUSE").ok();
    unsafe { std::env::remove_var("CISTELLA_LIVE_REFUSE") };
    let profile = worktree.path().join("accept-refuse.toml");
    std::fs::write(
        &profile,
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         container-home = \"/home/cistella\"\n\
         mounts = []\n\
         environment-acceptances = [\"CISTELLA_LIVE_REFUSE\"]\n",
    )
    .unwrap();
    // Spawn (not synchronous output()): if snapshotting ever regresses
    // below creation-lock acquisition, the child blocks on the test-held
    // lock instead of failing — poll to a deadline, kill/reap on timeout,
    // and fail explicitly. nextest sets no terminate-after bound, so an
    // unbounded wait would hang the live suite forever.
    let mut child = Command::new(bin())
        .args([
            "conduct",
            "--profile",
            &profile.to_string_lossy(),
            "--session-directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sleep",
            "300",
        ])
        .env("HOME", &home)
        .env("TERM", "xterm-ghostty")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn conduct");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll conduct") {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().ok();
            let _ = child.wait();
            panic!(
                "conduct reached the mutation gate (blocked on the creation lock): \
                 acceptance refusal must precede lock acquisition"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // try_wait reaped the child, so drain the captured pipes directly.
    use std::io::Read as _;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_end(&mut stderr)
        .unwrap();
    if let Some(v) = prev {
        unsafe { std::env::set_var("CISTELLA_LIVE_REFUSE", v) };
    }
    assert!(!status.success(), "absent acceptance refuses conduct");
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    assert!(
        stderr.contains("CISTELLA_LIVE_REFUSE"),
        "name-only diagnostic: {stderr}"
    );
    // The `conduct {id}` line prints after install/start/preparation, so
    // its absence proves no announced/started session (not pre-mint
    // ordering — mint itself is unobservable from outside).
    assert!(
        !String::from_utf8_lossy(&stdout).contains("conduct "),
        "refusal precedes session announce"
    );
    assert_eq!(cistella_unit_names(&home), units_before, "no unit residue");
    assert_eq!(
        cistella_scratch_names(),
        scratch_before,
        "no scratch residue"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn template_namespaces_resolve_live() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let home_var = std::env::var("HOME").expect("HOME set");

    // Supplement, environment, and early-home coverage: the profile takes
    // container-home from host HOME, a supplement-fed mount target, and
    // supplement/environment spans in env values and labels.
    let profile = worktree.path().join("tmpl-ns.toml");
    std::fs::write(
        &profile,
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         container-home = \"{{environment:HOME}}\"\n\
         [[mounts]]\n\
         host-source = \"/tmp\"\n\
         container-target = \"/ns-{{supplement:dataset}}\"\n\
         mode = \"ro\"\n\
         [environment-assignments]\n\
         PROBE_NS = \"{{supplement:dataset}}@{{environment:HOME}}\"\n\
         [labels]\n\
         \"ns.tag\" = \"{{core:project-name}}-{{supplement:dataset}}\"\n",
    )
    .unwrap();
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile.to_string_lossy(),
        &worktree_str,
        &[
            "--identity",
            "alice",
            "--project-name",
            "livenproj",
            "--supplement",
            "dataset=livedata",
        ],
        &["sleep", "300"],
        &[],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");

    // Early-home showcase: container HOME equals host HOME.
    let home_out = Command::new("podman")
        .args(["exec", &container, "sh", "-c", "echo $HOME"])
        .output()
        .expect("podman exec home");
    assert!(home_out.status.success(), "podman exec home");
    assert_eq!(
        String::from_utf8_lossy(&home_out.stdout).trim(),
        home_var,
        "container-home from host HOME"
    );
    // Supplement and environment spans resolve in-container.
    let env_out = Command::new("podman")
        .args(["exec", &container, "sh", "-c", "echo $PROBE_NS"])
        .output()
        .expect("podman exec env");
    assert!(env_out.status.success(), "podman exec env");
    assert_eq!(
        String::from_utf8_lossy(&env_out.stdout).trim(),
        format!("livedata@{home_var}"),
        "supplement/environment templates resolved in-container"
    );
    // Supplement-fed mount target and label round-trip.
    let mount_out = Command::new("podman")
        .args([
            "exec",
            &container,
            "sh",
            "-c",
            "test -d /ns-livedata && echo yes",
        ])
        .output()
        .expect("podman exec mount");
    assert_eq!(
        String::from_utf8_lossy(&mount_out.stdout).trim(),
        "yes",
        "supplement mount target exists"
    );
    assert_eq!(
        podman_label(&container, "ns.tag"),
        "livenproj-livedata",
        "supplement label resolved on container"
    );

    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = conduct.wait();
    guard.id = None;
    assert!(!unit_path(&home, &id).exists(), "no unit residue");
    assert!(scratch_gone(&id), "no scratch residue");
}
