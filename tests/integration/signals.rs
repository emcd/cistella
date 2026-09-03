//! Signals and races: startup abort, creation race, SIGHUP, lock window.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::helpers::*;

#[test]
fn signal_during_startup_tears_down() {
    // SIGTERM landing between install and start (opened deterministically by
    // the start-delay hook) must tear down the installed unit and scratch
    // and exit 128+SIGTERM instead of dying by default action.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let unit_dir = PathBuf::from(&home).join(".config/containers/systemd");
    let before: std::collections::HashSet<String> = std::fs::read_dir(&unit_dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();

    let mut child = Command::new(bin())
        .args([
            "conduct",
            "--profile",
            "default",
            "--directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "true",
        ])
        .env("HOME", &home)
        .env("TERM", "xterm-ghostty")
        .env("CISTELLA_CONDUCT_START_DELAY_MS", "15000")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn conduct");
    // The installed unit file marks the delay window (no id line is printed
    // on the abort path, so track the new file, not stdout).
    let (id, upath) = wait_for_unit_file(&unit_dir, &before, &worktree_str);
    let mut guard = Guard {
        id: Some(id.clone()),
    };

    let pid = child.id();
    Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .unwrap();
    let status = child.wait().expect("conduct reaped");
    assert_eq!(status.code(), Some(143), "128+SIGTERM, got {status:?}");
    guard.id = None;
    assert!(!upath.exists(), "installed unit removed by startup abort");
    assert!(scratch_gone(&id), "scratch removed by startup abort");
}

#[test]
fn terminate_races_creation_waits() {
    // Terminate issued while conduct is still installing must wait for the
    // creation lock, then tear down the fully installed session — never reap
    // a half-created unit out from under the start.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let unit_dir = PathBuf::from(&home).join(".config/containers/systemd");
    let before: std::collections::HashSet<String> = std::fs::read_dir(&unit_dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();

    let conduct = Command::new(bin())
        .args([
            "conduct",
            "--profile",
            "default",
            "--directory",
            &worktree_str,
            "--identity",
            "alice",
            "--",
            "sleep",
            "300",
        ])
        .env("HOME", &home)
        .env("TERM", "xterm-ghostty")
        .env("CISTELLA_CONDUCT_START_DELAY_MS", "15000")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn conduct");
    let (id, _upath) = wait_for_unit_file(&unit_dir, &before, &worktree_str);
    let mut guard = Guard {
        id: Some(id.clone()),
    };

    // Terminate by directory selector while start is still delayed: with the
    // lock held across scan-and-teardown this blocks until the session is
    // fully installed, then converges with the attached conduct.
    let out = run_cistella(&home, &["terminate", "--directory", &worktree_str]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let output = conduct.wait_with_output().expect("conduct reaped");
    guard.id = None;
    // The harness actually ran (killed with its container), conduct did not
    // die in start on a reaped unit.
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("start"),
        "conduct started cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(scratch_gone(&id), "scratch removed by raced teardown");
    let out = run_cistella(&home, &["survey", "--directory", &worktree_str]);
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&id));
}

#[test]
fn sighup_conduct_tears_down() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = TempDir::new().unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();

    let (mut conduct, id, mut guard) =
        spawn_conduct(&home, &worktree_str, &["--identity", "alice"]);
    wait_active(&id);
    let pid = conduct.id();
    let out = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let status = conduct.wait().expect("conduct reaped");
    assert_eq!(status.code(), Some(143), "128+SIGTERM, got {status:?}");
    guard.id = None;
    assert!(
        !unit_path(&home, &id).exists(),
        "no unit residue after signal"
    );
    assert!(scratch_gone(&id), "no scratch residue after signal");
}

#[test]
fn gc_reaps_handwritten_orphan_after_lock_release() {
    // Phase variant of the creation-window promise: hold the creation lock
    // for the whole test so fleet gc processes cannot steal the fixture,
    // prove a concurrent gc blocks, then reap in-process via the lock-held
    // gc entry point and verify unit + scratch are gone.
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let home = home_dir();
    let _held = cistella::lock::LockGuard::acquire().expect("acquire lock");
    let fake_id = "zz9phaseprobe00";
    let mut cleanup = Guard {
        id: Some(fake_id.to_string()),
    };
    let scratch = cistella::lock::scratch_dir(fake_id);
    std::fs::create_dir_all(&scratch).unwrap();
    let unit = unit_path(&home, fake_id);
    std::fs::write(
        &unit,
        format!(
            "[Unit]\nDescription=phase probe\n\n[Container]\nImage=localhost/cistella/opencode:example\nContainerName=cistella-{fake_id}\nLabel=cistella.id=\"{fake_id}\"\nExec=sleep infinity\n"
        ),
    )
    .unwrap();

    let mut gc = Command::new(bin())
        .arg("gc")
        .env("HOME", &home)
        .env("TERM", "xterm-ghostty")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn gc");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        gc.try_wait().expect("poll gc").is_none(),
        "gc blocks on the lock with a half-created session present"
    );
    gc.kill().ok();
    let _ = gc.wait();

    let res = cistella::runtime::gc_exited_locked().expect("locked gc");
    assert!(
        res.reaped
            .iter()
            .any(|n| n == &format!("cistella-{fake_id}")),
        "locked gc reaps the handwritten orphan: {:?}",
        res.reaped
    );
    cleanup.id = None;
    assert!(!unit.exists(), "orphan unit reaped");
    assert!(scratch_gone(fake_id), "orphan scratch reaped");
}

#[test]
fn gc_creation_window_reaps_nothing() {
    // Holding the creation-window lock blocks a concurrent gc: it finishes
    // only after the guard drops, having reaped nothing mid-install.
    let guard = cistella::lock::LockGuard::acquire().expect("acquire lock");
    assert!(
        cistella::lock::LockGuard::try_acquire()
            .expect("try")
            .is_none()
    );
    let mut gc = Command::new(bin())
        .arg("gc")
        .env("HOME", home_dir())
        .env("TERM", "xterm-ghostty")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn gc");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        gc.try_wait().expect("poll gc").is_none(),
        "gc blocks on the lock"
    );
    drop(guard);
    let out = gc.wait_with_output().expect("gc output");
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The integration binary runs serially (see .config/nextest.toml), so
    // no concurrent test holds the install lock; poll briefly regardless
    // for `cargo test` runners that ignore the nextest config.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut free = false;
    while Instant::now() <= deadline {
        if cistella::lock::LockGuard::try_acquire()
            .expect("try")
            .is_some()
        {
            free = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(free, "lock released after gc");
}
