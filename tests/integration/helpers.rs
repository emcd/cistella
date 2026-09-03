//! Shared helpers for lifecycle integration tests.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cistella")
}

pub fn systemd_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn home_dir() -> String {
    std::env::var("HOME").expect("HOME set")
}

pub fn run_cistella(home: &str, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .env("HOME", home)
        .env("TERM", "xterm-ghostty")
        .output()
        .expect("spawn cistella")
}

pub fn valid_minted_id(id: &str) -> bool {
    id.len() == cistella::session::MINTED_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

pub fn unit_path(home: &str, id: &str) -> PathBuf {
    PathBuf::from(home)
        .join(".config/containers/systemd")
        .join(format!("cistella-{id}.container"))
}

pub fn scratch_paths(id: &str) -> Vec<PathBuf> {
    vec![
        cistella::lock::scratch_dir(id),
        cistella::lock::legacy_scratch_dir(id),
    ]
}

pub fn scratch_gone(id: &str) -> bool {
    scratch_paths(id).iter().all(|p| !p.exists())
}

/// Guard removes the session even when an assertion fails.
pub struct Guard {
    pub id: Option<String>,
}

impl Guard {
    pub fn empty() -> Self {
        Self { id: None }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let home = home_dir();
            let _ = run_cistella(&home, &["terminate", &id]);
            for path in scratch_paths(&id) {
                let _ = std::fs::remove_dir_all(path);
            }
            let _ = run_cistella(&home, &["gc"]);
        }
    }
}

/// Spawns `conduct -- sleep 300` in the background and returns the child
/// plus the minted id from its first stdout line.
pub fn spawn_conduct(home: &str, worktree: &str, extra: &[&str]) -> (Child, String, Guard) {
    spawn_conduct_full(home, "default", worktree, extra, &["sleep", "300"], &[])
}

/// Spawns `conduct` with explicit profile, harness argv, and environment.
pub fn spawn_conduct_full(
    home: &str,
    profile: &str,
    worktree: &str,
    extra: &[&str],
    argv: &[&str],
    envs: &[(&str, &str)],
) -> (Child, String, Guard) {
    let mut cmd = Command::new(bin());
    cmd.args(["conduct", "--profile", profile, "--directory", worktree]);
    cmd.args(extra);
    cmd.arg("--");
    cmd.args(argv);
    cmd.env("HOME", home);
    cmd.env("TERM", "xterm-ghostty");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn conduct");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(60);
    let id = loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("timed out waiting for conduct id line");
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(_) => {
                let line = line.trim().to_string();
                if let Some(id) = line.strip_prefix("conduct ") {
                    assert!(valid_minted_id(id), "minted id shape: {id}");
                    break id.to_string();
                }
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    // Hand the reader back so the pipe stays drained; sleep outputs nothing.
    let _ = reader;
    (child, id.clone(), Guard { id: Some(id) })
}

pub fn wait_active(id: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let out = Command::new("systemctl")
            .args([
                "--user",
                "show",
                "-p",
                "ActiveState",
                &format!("cistella-{id}.service"),
            ])
            .output()
            .expect("systemctl show");
        let txt = String::from_utf8_lossy(&out.stdout).to_string();
        if txt.contains("ActiveState=active") {
            return;
        }
        if Instant::now() > deadline {
            panic!("service cistella-{id} never active: {txt}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn unit_label(home: &str, id: &str, key: &str) -> String {
    let content = std::fs::read_to_string(unit_path(home, id)).expect("unit readable");
    let prefix = format!("Label={key}=");
    let raw = content
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(|v| v.to_string()))
        .unwrap_or_else(|| panic!("unit missing {key}"));
    // Units store systemd-quoted values; the registry sees the unquoted form.
    cistella::runtime::unquote_systemd(&raw)
}

/// Reads the raw `cistella.directory` label from a unit file path.
pub fn unit_directory_label(path: &PathBuf) -> String {
    let prefix = "Label=cistella.directory=";
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix(prefix).map(|v| v.to_string()))
        .map(|v| cistella::runtime::unquote_systemd(&v))
        .unwrap_or_default()
}

/// Reads one podman label from the running container.
pub fn podman_label(container: &str, key: &str) -> String {
    let out = Command::new("podman")
        .args([
            "inspect",
            "--format",
            &format!("{{{{index .Config.Labels \"{key}\"}}}}"),
            container,
        ])
        .output()
        .expect("podman inspect");
    assert!(out.status.success(), "inspect {container}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn wait_for_unit_file(
    unit_dir: &PathBuf,
    before: &std::collections::HashSet<String>,
    worktree: &str,
) -> (String, PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(entries) = std::fs::read_dir(unit_dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("cistella-")
                    && name.ends_with(".container")
                    && !before.contains(&name)
                    && unit_directory_label(&e.path()) == worktree
                {
                    let id = name
                        .trim_start_matches("cistella-")
                        .trim_end_matches(".container")
                        .to_string();
                    return (id, e.path());
                }
            }
        }
        if Instant::now() > deadline {
            panic!("unit file never appeared for {worktree}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn enter_stty_via_pty(home: &str, worktree: &str) -> bool {
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
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(30) {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        if String::from_utf8_lossy(&acc).contains("DONE") {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
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
            eprintln!("enter pty acc: {txt}");
            txt.contains("30 100") || (txt.contains("DONE") && !txt.contains("0 0"))
        }
        Ok(ForkptyResult::Child) => {
            use std::os::unix::process::CommandExt;
            let home = home.to_string();
            let worktree = worktree.to_string();
            let e = Command::new(bin())
                .args([
                    "enter",
                    "--directory",
                    &worktree,
                    "--",
                    "sh",
                    "-c",
                    "stty size; echo DONE",
                ])
                .env("HOME", home)
                .env("TERM", "xterm-ghostty")
                .exec();
            eprintln!("cistella enter {e:?}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("forkpty {e}");
            false
        }
    }
}
