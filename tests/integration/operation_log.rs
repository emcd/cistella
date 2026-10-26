//! Operation-log live pins: gc decisions reconstructed from the log
//! on a real backend, and logger faults leaving gc identical.
//!
//! Both tests mirror the orphan-test lock discipline: the creation
//! lock is held across the kill and the presence assertions, then
//! DROPPED before invoking `gc` - a child gc must acquire the same
//! lock, so holding it across the spawn is a deterministic
//! self-deadlock (60s timeout, zero output). The reconstruction
//! scans every gc operation log under the shared root, so it stays
//! sound when a sibling gc reaps the fixture first: the log that
//! names this session is the receipt no matter which gc wrote it.

use std::process::Command;

use super::helpers::*;

/// Finds any gc operation log naming this session in a classify
/// record, and asserts the reap decision plus a success terminal
/// in the same log. Returns the gc operation id so the caller can
/// correlate the nested teardown it triggered.
fn assert_gc_logged_reap(id: &str) -> String {
    let root = cistella::operation_log::log_root();
    let mut found = String::new();
    let listing = std::fs::read_dir(&root).expect("log root readable");
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !cistella::operation_log::op_id_valid(&name) {
            continue;
        }
        let read = match cistella::operation_log::read_operation(&entry.path()) {
            Ok(read) => read,
            Err(_) => continue,
        };
        let mut classified = false;
        let mut terminal_ok = false;
        for record in &read.records {
            if record.get("phase").and_then(|v| v.as_str()) != Some("classify") {
                if record.get("kind").and_then(|v| v.as_str()) == Some("terminal") {
                    terminal_ok = record.get("outcome").and_then(|v| v.as_str()) == Some("success");
                }
                continue;
            }
            let sid = record.get("sid").and_then(|v| v.as_str()).unwrap_or("");
            let decision = record
                .get("decision")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if sid == id && decision == "reap-orphan-unit" {
                classified = true;
            }
        }
        if classified {
            assert!(
                terminal_ok,
                "reap decision rides a success terminal for {id}"
            );
            found = entry.file_name().to_string_lossy().to_string();
        }
    }
    assert!(
        !found.is_empty(),
        "gc operation log reconstructs the reap of {id}"
    );
    found
}

/// Whether one teardown child log targets this session and
/// container: it names the container on a unit-removed record and
/// the session on an attestation record. Pure over the records so
/// the sibling filtering below is pinned without a backend.
fn teardown_targets(records: &[serde_json::Value], id: &str, container: &str) -> bool {
    let mut saw_unit = false;
    let mut saw_session = false;
    for record in records {
        match record.get("step").and_then(|v| v.as_str()) {
            Some("unit-removed")
                if record.get("container").and_then(|v| v.as_str()) == Some(container) =>
            {
                saw_unit = true;
            }
            Some("attestation-reaped")
                if record.get("session").and_then(|v| v.as_str()) == Some(id) =>
            {
                saw_session = true;
            }
            _ => {}
        }
    }
    saw_unit && saw_session
}

/// Asserts a teardown operation linked to the gc run recorded the
/// stop and the unlink outcome, including the reload result from
/// the actual unlink call rather than a pre-check. The teardown
/// must name the expected session and container: a sibling gc's
/// teardown for another session does not satisfy this receipt.
fn assert_teardown_logged_reload(gc_id: &str, id: &str, container: &str) {
    let root = cistella::operation_log::log_root();
    let mut found = false;
    let listing = std::fs::read_dir(&root).expect("log root readable");
    for entry in listing.flatten() {
        let read = match cistella::operation_log::read_operation(&entry.path()) {
            Ok(read) => read,
            Err(_) => continue,
        };
        let start = match read.records.first() {
            Some(start) => start,
            None => continue,
        };
        let is_teardown = start.get("actor").and_then(|v| v.as_str()) == Some("teardown");
        let parent = start.get("parent").and_then(|v| v.as_str()).unwrap_or("");
        if !is_teardown || parent != gc_id {
            continue;
        }
        // Filter BEFORE asserting: a multi-target gc links several
        // teardowns to this parent, and an unrelated sibling must
        // neither satisfy nor fail this receipt. Only the child
        // naming this session and container is asserted.
        if !teardown_targets(&read.records, id, container) {
            continue;
        }
        let mut saw_stop = false;
        let mut reload = None;
        for record in &read.records {
            match record.get("step").and_then(|v| v.as_str()) {
                Some("stop-settled") => {
                    saw_stop = true;
                }
                Some("unit-removed")
                    if record.get("container").and_then(|v| v.as_str()) == Some(container) =>
                {
                    reload = record.get("reload").and_then(|v| v.as_str());
                }
                _ => {}
            }
        }
        let reload = reload.expect("teardown records its reload outcome");
        assert!(
            reload == "ok" || reload == "not-required",
            "reload reflects the actual unlink call, got: {reload}"
        );
        assert!(saw_stop, "teardown records its stop");
        found = true;
    }
    assert!(found, "gc-linked teardown recorded stop and reload");
}

#[test]
fn teardown_filter_matches_only_the_target_sibling() {
    // Backend-free control for the multi-target helper: an
    // unrelated sibling teardown (full stop/unit/attestation of
    // its own) must not satisfy the target, in either sibling
    // order, and the target must satisfy even beside it. This
    // models the helper loop: filter every child, keep matches.
    let sibling = |session: &str, unit: &str| {
        vec![
            serde_json::json!({"step": "stop-settled"}),
            serde_json::json!({"step": "unit-removed", "container": unit, "reload": "ok"}),
            serde_json::json!({"step": "attestation-reaped", "session": session}),
        ]
    };
    let target = sibling("sess-target", "cistella-target");
    let other = sibling("sess-other", "cistella-other");
    for children in [&[&target, &other], &[&other, &target]] {
        let matched: Vec<&&Vec<serde_json::Value>> = children
            .iter()
            .filter(|records| teardown_targets(records, "sess-target", "cistella-target"))
            .collect();
        assert_eq!(matched.len(), 1, "exactly the target child matches");
        assert!(matched[0][1].get("container").and_then(|v| v.as_str()) == Some("cistella-target"));
    }
    // A sibling with a stop but no target identity never matches.
    assert!(!teardown_targets(
        &[serde_json::json!({"step": "stop-settled"})],
        "sess-target",
        "cistella-target"
    ));
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn gc_operation_log_reconstructs_reap() {
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (mut conduct, id, mut guard) =
        spawn_conduct(&home, &worktree_str, &["--identity", "oplog"]);
    wait_active(&id);
    // Freeze out sibling gc across the kill and the presence
    // assertions only: the orphan must still be present when the
    // test below reads it. The freeze drops before invoking gc -
    // the child must acquire the same lock.
    let _freeze = cistella::lock::LockGuard::acquire().expect("lock acquires");
    conduct.kill().expect("kill conduct");
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("reap conduct post-kill");
    guard.id = None;
    assert!(unit_path(&home, &id).exists(), "orphan unit remains");
    // External stop simulating a crash: the container is gone
    // (--rm) while the orphaned unit file remains, so the unit is
    // inactive and gc-eligible. Killing the driver alone leaves a
    // live container behind, which gc correctly refuses to reap.
    let container = format!("cistella-{id}");
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "stop", &format!("{container}.service")]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("stop runs");
    assert!(
        out.status.success(),
        "stop {container}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        unit_path(&home, &id).exists(),
        "unit file outlives external stop"
    );
    drop(_freeze);
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!unit_path(&home, &id).exists(), "gc removes orphan unit");
    let gc_id = assert_gc_logged_reap(&id);
    assert_teardown_logged_reload(&gc_id, &id, &container);
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn gc_with_unwritable_log_root_reaps_identically() {
    // Deterministic logger fault on the real gc path, without
    // permission games: XDG_STATE_HOME points at a regular file in
    // the child process only (no test-process env mutation), so the
    // log open fails and gc proceeds with identical decisions.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (mut conduct, id, mut guard) =
        spawn_conduct(&home, &worktree_str, &["--identity", "oplog-fault"]);
    wait_active(&id);
    let _freeze = cistella::lock::LockGuard::acquire().expect("lock acquires");
    conduct.kill().expect("kill conduct");
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("reap conduct post-kill");
    guard.id = None;
    assert!(unit_path(&home, &id).exists(), "orphan unit remains");
    // Same external stop as above: a killed driver alone leaves a
    // live container, which gc correctly refuses to reap.
    let container = format!("cistella-{id}");
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "stop", &format!("{container}.service")]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("stop runs");
    assert!(
        out.status.success(),
        "stop {container}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        unit_path(&home, &id).exists(),
        "unit file outlives external stop"
    );
    drop(_freeze);
    let blocker = tempfile::NamedTempFile::new().expect("blocker file");
    let mut cmd = Command::new(bin());
    cmd.arg("gc");
    cmd.env("HOME", &home);
    cmd.env("TERM", "xterm-ghostty");
    cmd.env("XDG_STATE_HOME", blocker.path());
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("spawn gc");
    assert!(
        out.status.success(),
        "gc without logging: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !unit_path(&home, &id).exists(),
        "gc reaps identically without its log"
    );
}
