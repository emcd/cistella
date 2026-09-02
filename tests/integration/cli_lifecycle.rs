//! CLI lifecycle: run → status → exec → stop → gc (Quadlet, journal fallback).
//! Real systemd test when user manager is available; otherwise skip.

use std::process::Command;
use tempfile::TempDir;

fn systemd_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run_cistella_with_home(home: &str, _worktree: &str, args: &[&str]) -> std::process::Output {
    Command::new("cargo")
        .args(["run", "--quiet", "--bin", "cistella", "--"])
        .args(args)
        .env("HOME", home)
        .env("TERM", "xterm-ghostty")
        .output()
        .expect("cargo run cistella")
}

#[test]
fn cli_run_stop_lifecycle() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = std::env::var("HOME").unwrap();
    let session = format!("itest-a-b-{}-{}", std::process::id(), {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000
    });
    let harness = "opencode";
    let seat = "alice";
    let profile = "default";

    // Ensure cleanup guard even on failure
    struct Guard {
        home: String,
        session: String,
        harness: String,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _name = format!("cistella-{}-{}", self.harness, self.session);
            let _ = Command::new("cargo")
                .args([
                    "run",
                    "--quiet",
                    "--bin",
                    "cistella",
                    "--",
                    "stop",
                    "--session-id",
                    &self.session,
                    "--harness",
                    &self.harness,
                ])
                .env("HOME", &self.home)
                .output();
            let _ = std::fs::remove_dir_all(format!("/tmp/cistella-{}", self.session));
        }
    }
    let _guard = Guard {
        home: home.clone(),
        session: session.clone(),
        harness: harness.to_string(),
    };

    // run
    let out = run_cistella_with_home(
        &home,
        &worktree_str,
        &[
            "run",
            "--session-id",
            &session,
            "--seat",
            seat,
            "--harness",
            harness,
            "--profile",
            profile,
            "--worktree",
            &worktree_str,
        ],
    );
    eprintln!("run stdout: {}", String::from_utf8_lossy(&out.stdout));
    eprintln!("run stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.status.success(),
        "cistella run must succeed on systemd host: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let unit_path = std::path::Path::new(&home)
        .join(".config/containers/systemd")
        .join(format!("cistella-{harness}-{session}.container"));
    assert!(unit_path.exists(), "unit missing after run");
    let content = std::fs::read_to_string(&unit_path).unwrap();
    assert!(content.contains("Tmpfs=/home/cistella"));
    assert!(content.contains("SuccessExitStatus=143"));
    assert!(!content.contains("Environment=TERM="));

    // status shows Up
    let out = run_cistella_with_home(&home, &worktree_str, &["status"]);
    assert!(out.status.success());
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        txt.contains(&session) || txt.contains(harness),
        "status should contain session: {txt}"
    );

    // exec via cistella exec under PTY (proves driver transport wiring, not just podman)
    let cname = format!("cistella-{harness}-{session}");
    let exec_ok = exec_stty_via_cistella_pty(&home, &session, harness);
    assert!(exec_ok, "cistella exec stty via pty failed");

    // stop
    let out = run_cistella_with_home(
        &home,
        &worktree_str,
        &["stop", "--session-id", &session, "--harness", harness],
    );
    eprintln!("stop stdout: {}", String::from_utf8_lossy(&out.stdout));
    eprintln!("stop stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "stop must succeed");

    // Assert: ActiveState inactive, unit file gone, scratch gone, NOT in --failed
    let show = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "ActiveState",
            "-p",
            "LoadState",
            &format!("{cname}.service"),
        ])
        .output()
        .unwrap();
    let show_txt = String::from_utf8_lossy(&show.stdout).to_string();
    eprintln!("show after stop: {show_txt}");
    assert!(
        show_txt.contains("ActiveState=inactive") || show_txt.contains("LoadState=not-found"),
        "service should be inactive/not-found after stop: {show_txt}"
    );
    assert!(
        !unit_path.exists(),
        "unit file should be removed after stop"
    );
    // Scratch must be removed by stop (no manual pre-cleanup); this is the hyphenated-session regression.
    assert!(
        !std::path::Path::new(&format!("/tmp/cistella-{session}")).exists(),
        "scratch /tmp/cistella-{session} should be removed by stop (hyphenated id)"
    );
    let failed = Command::new("systemctl")
        .args(["--user", "--failed", "--no-legend"])
        .output()
        .unwrap();
    let failed_txt = String::from_utf8_lossy(&failed.stdout).to_string();
    assert!(
        !failed_txt.contains(&cname),
        "service should not be in --failed after reset-failed: {failed_txt}"
    );
}

#[test]
fn gc_reaps_orphan_after_external_stop() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = std::env::var("HOME").unwrap();
    let session = format!("itest-a-b-{}-{}", std::process::id(), {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000
    });
    let harness = "opencode";
    let seat = "alice";
    let profile = "default";
    struct Guard {
        home: String,
        session: String,
        harness: String,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = Command::new("cargo")
                .args([
                    "run",
                    "--quiet",
                    "--bin",
                    "cistella",
                    "--",
                    "stop",
                    "--session-id",
                    &self.session,
                    "--harness",
                    &self.harness,
                ])
                .env("HOME", &self.home)
                .output();
            let _ = std::fs::remove_dir_all(format!("/tmp/cistella-{}", self.session));
            let _ = Command::new("cargo")
                .args(["run", "--quiet", "--bin", "cistella", "--", "gc"])
                .env("HOME", &self.home)
                .output();
        }
    }
    let _guard = Guard {
        home: home.clone(),
        session: session.clone(),
        harness: harness.to_string(),
    };
    // run
    let out = run_cistella_with_home(
        &home,
        &worktree_str,
        &[
            "run",
            "--session-id",
            &session,
            "--seat",
            seat,
            "--harness",
            harness,
            "--profile",
            profile,
            "--worktree",
            &worktree_str,
        ],
    );
    assert!(
        out.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cname = format!("cistella-{harness}-{session}");
    let unit_path = std::path::Path::new(&home)
        .join(".config/containers/systemd")
        .join(format!("{cname}.container"));
    assert!(unit_path.exists());
    let scratch = format!("/tmp/cistella-{session}");
    assert!(
        std::path::Path::new(&scratch).exists(),
        "scratch should exist after run"
    );
    // External stop simulating crash
    let out = Command::new("systemctl")
        .args(["--user", "stop", &format!("{cname}.service")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "systemctl stop failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    // Container should be gone due to --rm, but unit file + scratch remain
    assert!(
        unit_path.exists(),
        "unit file should remain after external stop (orphan)"
    );
    assert!(
        std::path::Path::new(&scratch).exists(),
        "scratch should remain after external stop"
    );
    // gc should reap orphan
    let out = run_cistella_with_home(&home, &worktree_str, &["gc"]);
    eprintln!("gc stdout: {}", String::from_utf8_lossy(&out.stdout));
    eprintln!("gc stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.status.success(),
        "gc failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!unit_path.exists(), "gc should remove orphan unit file");
    assert!(
        !std::path::Path::new(&scratch).exists(),
        "gc should remove orphan scratch /tmp/cistella-{session}"
    );
}

fn exec_stty_via_cistella_pty(home: &str, session: &str, harness: &str) -> bool {
    use nix::pty::{ForkptyResult, Winsize, forkpty};
    use nix::sys::wait::{self, WaitStatus};
    use std::io::Read;
    let winsize = Winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    match unsafe { forkpty(Some(&winsize), None) } {
        Ok(ForkptyResult::Parent { child, master }) => {
            let mut f = std::fs::File::from(master);
            let mut buf = [0u8; 4096];
            let mut acc = Vec::new();
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_secs(5) {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        if String::from_utf8_lossy(&acc).contains("DONE") {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
                if wait::waitpid(child, Some(wait::WaitPidFlag::WNOHANG))
                    .is_ok_and(|s| s != WaitStatus::StillAlive)
                {
                    break;
                }
            }
            let _ = wait::waitpid(child, None);
            let txt = String::from_utf8_lossy(&acc).to_string();
            eprintln!("exec pty acc: {txt}");
            txt.contains("30 100") || (txt.contains("DONE") && !txt.contains("0 0"))
        }
        Ok(ForkptyResult::Child) => {
            use std::os::unix::process::CommandExt;
            let home = home.to_string();
            let session = session.to_string();
            let harness = harness.to_string();
            let e = Command::new("cargo")
                .args([
                    "run",
                    "--quiet",
                    "--bin",
                    "cistella",
                    "--",
                    "exec",
                    "--session-id",
                    &session,
                    "--harness",
                    &harness,
                    "--",
                    "sh",
                    "-c",
                    "stty size; echo DONE",
                ])
                .env("HOME", home)
                .env("TERM", "xterm-ghostty")
                .exec();
            eprintln!("cistella exec {e:?}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("forkpty {e}");
            false
        }
    }
}
