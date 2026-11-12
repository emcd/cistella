//! Signals and races: startup abort, creation race, SIGHUP, lock window.

use std::process::{Command, Stdio};
use std::time::Duration;

use super::helpers::*;
use super::parked::ParkedConduct;

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn signal_during_startup_tears_down() {
    // SIGTERM landing between install and start (held open by the
    // start-gate hook, never by elapsed time) must tear down the
    // installed unit and exit 128+SIGTERM instead of dying by
    // default action.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let fixture = fixture_profile("default.toml");
    // Causal pre-initiate hold: the gate parks the conduct after
    // install (creation IS the release, which never comes), so the
    // SIGTERM provably lands in the install/start window no matter
    // how slow the runner is — no elapsed-time premise.
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &fixture, &gate);
    let id = parked.discover_id();
    let upath = unit_path(&home, &id);
    assert!(upath.exists(), "installed unit marks the hold window");
    let status = parked.abort();
    assert_eq!(status.code(), Some(143), "128+SIGTERM, got {status:?}");
    assert!(!upath.exists(), "installed unit removed by startup abort");
    assert!(rdv_gone(&id), "rendezvous dir reaped on startup abort");
    parked.settle();
}

/// Panic-safe ownership for a racing terminator child: on ANY unwind
/// path the terminator is killed and reaped so it cannot stay blocked
/// on the creation lock. The parked conduct next to it is owned by
/// ParkedConduct, whose Drop kills it (releasing the lock) and
/// converges the unit — so no gate release is needed on failure
/// paths either. Past `join`, `wait_bounded` already killed and
/// reaped inside its own deadline (self-cleaning), so only the
/// pre-join window needs this guard. Disarm is implicit: a joined
/// child leaves nothing owned.
struct TerminatorHold {
    child: Option<std::process::Child>,
    done: bool,
}

impl TerminatorHold {
    fn spawn(home: &str, worktree: &str) -> Self {
        let child = Command::new(bin())
            .args(["terminate", "--directory", worktree])
            .env("HOME", home)
            .env("TERM", "xterm-ghostty")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn racing terminate");
        Self {
            child: Some(child),
            done: false,
        }
    }

    fn child(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("terminator owned until join")
    }

    fn join(mut self) -> std::process::Output {
        let child = self.child.take().expect("terminator owned until join");
        let out = wait_bounded(child, RUN_CISTELLA_TIMEOUT).expect("terminator reaped");
        self.done = true;
        out
    }
}

impl Drop for TerminatorHold {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
        }
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn terminate_waits_on_creation_lock_pre_initiate() {
    // Terminate issued while conduct is held pre-initiate must block
    // on the creation lock — proven, not slept-for: the actual child
    // shows as a blocked waiter on the exact lock (pid + dev:ino)
    // with the process observably alive, all before release. Only
    // then is the gate released and the fully installed session torn
    // down. Asserted: termination success, survey success with the
    // session absent, unit convergence. Conduct's own exit is recorded,
    // not pinned to a branch: harness execution is witnessed on no
    // path here, and only witnessed branches may claim it. (The
    // sleep-300 harness cannot exit 0 inside a bounded test, so a
    // success status would mean the harness never ran AND conduct
    // reported clean — that is the one ruled-out outcome.)
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let fixture = fixture_profile("default.toml");
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");

    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &fixture, &gate);
    let id = parked.discover_id();
    let upath = unit_path(&home, &id);
    assert!(upath.exists(), "installed unit marks the hold window");

    // The actual terminator child (not a helper): lock contention is
    // observable on THIS pid.
    let mut holder = TerminatorHold::spawn(&home, &worktree_str);
    wait_lock_blocked(holder.child(), Duration::from_secs(10));
    assert!(
        holder
            .child()
            .try_wait()
            .expect("poll terminator")
            .is_none(),
        "terminator blocks on the creation lock with the session half-installed"
    );
    std::fs::write(&gate, b"release").expect("release the gate");
    let out = holder.join();
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let output = parked.reap_output();
    eprintln!(
        "conduct exit after raced teardown (record, not verdict): {}",
        output.status
    );
    assert!(
        !output.status.success(),
        "sleep-300 harness cannot exit 0 inside a bounded test: {}",
        output.status
    );
    assert!(!upath.exists(), "unit removed by raced teardown");
    assert!(rdv_gone(&id), "rendezvous dir reaped by raced teardown");
    let survey = run_cistella(&home, &["survey", "--directory", &worktree_str]);
    assert!(
        survey.status.success(),
        "survey runs: {}",
        String::from_utf8_lossy(&survey.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&survey.stdout).contains(&id),
        "torn-down session absent from survey"
    );
    parked.settle();
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn sighup_conduct_tears_down() {
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();

    let (mut conduct, id, mut guard) =
        spawn_conduct(&home, &worktree_str, &["--identity", "alice"]);
    let container = format!("cistella-{id}");
    // Gate the HUP on launch commitment (attestation appearance),
    // not service-active: HUP during startup takes the abort path
    // (different disposition), so the 129 pin needs post-launch.
    wait_attestation(&id, &container);
    let pid = conduct.id();
    // SIGHUP (not SIGTERM — startup-abort already pins the TERM
    // path): the conduct-level HUP trap tears down, exit 128+1.
    let mut cmd = Command::new("kill");
    cmd.args(["-HUP", &pid.to_string()]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).unwrap();
    assert!(
        out.status.success(),
        "kill -HUP {pid}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("conduct reaped");
    assert_eq!(status.code(), Some(129), "128+SIGHUP, got {status:?}");
    guard.id = None;
    assert!(
        !unit_path(&home, &id).exists(),
        "no unit residue after signal"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn gc_reaps_handwritten_orphan_after_lock_release() {
    // Phase variant of the creation-window promise: hold the creation lock
    // for the whole test so fleet gc processes cannot steal the fixture,
    // prove a concurrent gc blocks, then reap in-process via the lock-held
    // gc entry point and verify the unit is gone.
    if !systemd_gate() {
        return;
    }
    let home = home_dir();
    let _held = cistella::lock::LockGuard::acquire().expect("acquire lock");
    let fake_id = "zz9phaseprobe00";
    let mut cleanup = Guard {
        id: Some(fake_id.to_string()),
    };
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
    // Lock-contention synchronization (no sleep-tuned settle):
    // /proc/locks must show this gc blocked on this exact lock
    // file (pid + dev:ino), so the observation proves contention
    // — not mere fd-presence, which a bypassed flock would also
    // produce. try_wait after that proves the block is live
    // (an unscheduled child shows no blocked entry). Test-side
    // lock is held throughout.
    wait_lock_blocked(&mut gc, Duration::from_secs(10));
    // Blockage is structural (lock held ⇒ gc cannot proceed),
    // so no settle sleep: try_wait after a proven attempt is
    // correct in every case (blocked → None passes; crashed →
    // Some fails loud). The lock is the synchronization.
    assert!(
        gc.try_wait().expect("poll gc").is_none(),
        "gc blocks on the lock with a half-created session present"
    );
    gc.kill().ok();
    let _ = reap_bounded(&mut gc, CHILD_REAP_TIMEOUT)
        .expect("reap gc post-kill (missing reap is wedged-child evidence)");

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
}

#[ignore = "live: requires systemd user manager and podman"]
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
    // Same lock-contention synchronization as above.
    wait_lock_blocked(&mut gc, Duration::from_secs(10));
    // Same structural blockage as above: no settle sleep.
    assert!(
        gc.try_wait().expect("poll gc").is_none(),
        "gc blocks on the lock"
    );
    drop(guard);
    let out = wait_bounded(gc, CHILD_REAP_TIMEOUT).expect("gc output");
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
