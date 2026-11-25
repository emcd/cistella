//! Operation-log live pins: gc decisions reconstructed from the log
//! on a real backend, and logger faults leaving gc identical.
//!
//! Both tests scope their gc invocations to their own fixtures
//! (`CISTELLA_GC_SCOPE`), so concurrent suites never observe
//! each other's orphans — including the unwritable-log-root
//! path, whose gc reaps without leaving receipts by design. The
//! reconstruction scan still tolerates receipted sibling reaps
//! (the log naming this session is the receipt no matter which
//! gc wrote it); scoping prevents the receipt-less cross-reap
//! the scan cannot see. The shared-vs-isolated control below
//! demonstrates both halves under deliberate overlap.
//!
//! All three tests mirror the orphan-test lock discipline: the
//! creation lock is held across the kill and the presence
//! assertions, then DROPPED before invoking `gc` - a child gc
//! must acquire the same lock, so holding it across the spawn
//! is a deterministic self-deadlock (60s timeout, zero output).

use std::process::Command;

use super::helpers::*;

/// gc with explicit child env and no test-process mutation: the
/// scope isolates this gc to the named sessions (its own
/// fixtures), and an optional state-home override points the
/// operation log at a blocker file (logger fault, decisions
/// identical, zero receipts).
fn run_gc_isolated(
    home: &str,
    scope: &str,
    state_home: Option<&std::path::Path>,
) -> std::process::Output {
    let mut cmd = spawn_cistella_cmd(home, &["gc"]);
    cmd.env(cistella::isolators::quadlet_gc::GC_SCOPE_ENV, scope);
    if let Some(root) = state_home {
        cmd.env("XDG_STATE_HOME", root);
    }
    run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("spawn gc")
}

/// Asserts no gc operation log classifies this session as a
/// reap: the isolation boundary held and no sibling observed
/// the fixture. Mirror of the reconstruction scan, expecting
/// absence.
fn assert_no_gc_logged_reap(id: &str) {
    let root = cistella::operation_log::log_root();
    let listing = std::fs::read_dir(&root).expect("log root readable");
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !cistella::operation_log::op_id_valid(&name) {
            continue;
        }
        let read = match cistella::operation_log::read_operation(&entry.path())
        {
            Ok(read) => read,
            Err(_) => continue,
        };
        for record in &read.records {
            if record.get("phase").and_then(|v| v.as_str()) != Some("classify")
            {
                continue;
            }
            let sid = record.get("sid").and_then(|v| v.as_str()).unwrap_or("");
            let decision = record
                .get("decision")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert!(
                !(sid == id && decision == "reap-orphan-unit"),
                "scoped-out session {id} observed by a gc"
            );
        }
    }
}

/// Builds one gc-eligible orphan: conduct, kill, external stop,
/// presence assertions. Mirrors the two existing tests' setup;
/// the caller owns the creation-lock discipline around gc.
fn make_orphan(
    home: &str,
    worktree: &str,
    identity: &str,
) -> (String, String) {
    let (mut conduct, id, mut guard) =
        spawn_conduct(home, worktree, &["--identity", identity]);
    wait_active(&id);
    let _freeze = cistella::lock::LockGuard::acquire().expect("lock acquires");
    conduct.kill().expect("kill conduct");
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT)
        .expect("reap conduct post-kill");
    guard.id = None;
    assert!(unit_path(home, &id).exists(), "orphan unit remains");
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
        unit_path(home, &id).exists(),
        "unit file outlives external stop"
    );
    (id, container)
}

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
        let read = match cistella::operation_log::read_operation(&entry.path())
        {
            Ok(read) => read,
            Err(_) => continue,
        };
        let mut classified = false;
        let mut terminal_ok = false;
        for record in &read.records {
            if record.get("phase").and_then(|v| v.as_str()) != Some("classify")
            {
                if record.get("kind").and_then(|v| v.as_str())
                    == Some("terminal")
                {
                    terminal_ok =
                        record.get("outcome").and_then(|v| v.as_str())
                            == Some("success");
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
fn teardown_targets(
    records: &[serde_json::Value],
    id: &str,
    container: &str,
) -> bool {
    let mut saw_unit = false;
    let mut saw_session = false;
    for record in records {
        match record.get("step").and_then(|v| v.as_str()) {
            Some("unit-removed")
                if record.get("container").and_then(|v| v.as_str())
                    == Some(container) =>
            {
                saw_unit = true;
            },
            Some("attestation-reaped")
                if record.get("session").and_then(|v| v.as_str())
                    == Some(id) =>
            {
                saw_session = true;
            },
            _ => {},
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
        let read = match cistella::operation_log::read_operation(&entry.path())
        {
            Ok(read) => read,
            Err(_) => continue,
        };
        let start = match read.records.first() {
            Some(start) => start,
            None => continue,
        };
        let is_teardown =
            start.get("actor").and_then(|v| v.as_str()) == Some("teardown");
        let parent =
            start.get("parent").and_then(|v| v.as_str()).unwrap_or("");
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
                },
                Some("unit-removed")
                    if record.get("container").and_then(|v| v.as_str())
                        == Some(container) =>
                {
                    reload = record.get("reload").and_then(|v| v.as_str());
                },
                _ => {},
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
            .filter(|records| {
                teardown_targets(records, "sess-target", "cistella-target")
            })
            .collect();
        assert_eq!(matched.len(), 1, "exactly the target child matches");
        assert!(
            matched[0][1].get("container").and_then(|v| v.as_str())
                == Some("cistella-target")
        );
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
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT)
        .expect("reap conduct post-kill");
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
    let out = run_gc_isolated(&home, &id, None);
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
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT)
        .expect("reap conduct post-kill");
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
    let out = run_gc_isolated(&home, &id, Some(blocker.path()));
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

/// Shared-vs-isolated control under deliberate overlap: the same
/// two-orphan layout interferes under shared scope and stays
/// clean under isolated scope. Sibling gc invocations stand in
/// for the concurrent suite actor that produced the historical
/// receipt-less removal; the owning gc stays scoped to its own
/// fixture throughout.
#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn gc_scope_isolates_sibling_fixtures() {
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    // Phase 1 (shared scope): an unscoped sibling gc observes and
    // reaps the victim orphan, with receipts. This is the
    // historical interference, demonstrated receipted: the
    // victim's own gc never runs, yet the unit is gone and the
    // log names it.
    let (victim_shared, container_shared) =
        make_orphan(&home, &worktree_str, "scope-shared");
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "shared sibling gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !unit_path(&home, &victim_shared).exists(),
        "shared sibling gc observes the victim"
    );
    let shared_gc = assert_gc_logged_reap(&victim_shared);
    assert_teardown_logged_reload(
        &shared_gc,
        &victim_shared,
        &container_shared,
    );
    // Phase 2 (isolated scope): the victim and sibling orphans
    // coexist, but the sibling gc is scoped to its own session
    // with an unopenable log root — the exact historical shape.
    // The victim must survive unobserved: unit remains, and no
    // classify record names it anywhere.
    let (sibling, _) = make_orphan(&home, &worktree_str, "scope-sibling");
    let (victim_isolated, container_isolated) =
        make_orphan(&home, &worktree_str, "scope-victim");
    // Unwritable root: the exact historical receipt-less shape.
    // Scope alone must protect the victim; the log absence is
    // expected here, not evidence.
    let blocker = tempfile::NamedTempFile::new().expect("blocker file");
    let out = run_gc_isolated(&home, &sibling, Some(blocker.path()));
    assert!(
        out.status.success(),
        "isolated sibling gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !unit_path(&home, &sibling).exists(),
        "scoped gc still reaps its own orphan"
    );
    assert!(
        unit_path(&home, &victim_isolated).exists(),
        "scoped sibling gc leaves the victim alone"
    );
    assert_no_gc_logged_reap(&victim_isolated);
    // The victim's owning gc, scoped to its own session, reaps
    // with the full receipt chain intact.
    let out = run_gc_isolated(&home, &victim_isolated, None);
    assert!(
        out.status.success(),
        "owning gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !unit_path(&home, &victim_isolated).exists(),
        "owning gc reaps its orphan"
    );
    let gc_id = assert_gc_logged_reap(&victim_isolated);
    assert_teardown_logged_reload(
        &gc_id,
        &victim_isolated,
        &container_isolated,
    );
}
