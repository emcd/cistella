// Orchestration pins through the injected seams: the fake
// runner replays scripted subprocess outcomes and records
// argv plus deadlines; the manual clock advances
// deterministically (no real sleeping past the cadence).
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt;

fn fake_output(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: std::process::ExitStatus::from_raw(if code == 0 { 0 } else { code << 8 }),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

fn ok(code: i32, stdout: &str, stderr: &str) -> Result<Output, BoundedRunError> {
    Ok(fake_output(code, stdout, stderr))
}

fn same_inspect() -> Result<Output, BoundedRunError> {
    ok(0, "abc123 2026-10-26 00:00:00 +0000 UTC\n", "")
}

fn changed_inspect() -> Result<Output, BoundedRunError> {
    ok(0, "abc123 2026-10-26 00:01:00 +0000 UTC\n", "")
}

fn timeout_with(partial_out: &str, partial_err: &str) -> Result<Output, BoundedRunError> {
    Err(BoundedRunError::Timeout {
        waited: Duration::from_secs(5),
        reaped: true,
        partial_stdout: partial_out.to_string(),
        partial_stderr: partial_err.to_string(),
    })
}

struct FakeRunner {
    script: RefCell<VecDeque<Result<Output, BoundedRunError>>>,
    recorded: RefCell<Vec<(Vec<String>, Instant)>>,
}

impl FakeRunner {
    fn new(script: Vec<Result<Output, BoundedRunError>>) -> Self {
        Self {
            script: RefCell::new(script.into()),
            recorded: RefCell::new(Vec::new()),
        }
    }

    fn argv_seen(&self) -> Vec<Vec<String>> {
        self.recorded
            .borrow()
            .iter()
            .map(|(a, _)| a.clone())
            .collect()
    }
}

impl ProbeRunner for FakeRunner {
    fn run(&self, argv: &[String], deadline: Instant) -> Result<Output, BoundedRunError> {
        self.recorded.borrow_mut().push((argv.to_vec(), deadline));
        self.script
            .borrow_mut()
            .pop_front()
            .expect("script exhausted")
    }
}

struct ManualClock {
    t: Cell<Instant>,
    steps: RefCell<std::collections::VecDeque<Duration>>,
    fallback: Duration,
}

impl ManualClock {
    fn new(step: Duration) -> Self {
        Self::scripted(vec![], step)
    }

    /// Deterministic schedule: the i-th `now()` call
    /// advances by `steps[i]`; exhausted schedules hold
    /// at `fallback` (usually zero: frozen time).
    fn scripted(steps: Vec<Duration>, fallback: Duration) -> Self {
        Self {
            t: Cell::new(Instant::now()),
            steps: RefCell::new(steps.into()),
            fallback,
        }
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        let step = self.steps.borrow_mut().pop_front().unwrap_or(self.fallback);
        let t = self.t.get();
        self.t.set(t + step);
        t
    }
}

const BOUND_ID: &str = "abc123";
const BOUND_STARTED: &str = "2026-10-26 00:00:00 +0000 UTC";

/// Realistic multi-property `systemctl show` output: every
/// explicitly selected property present, so rendering
/// truncation would drop a named assertion below.
fn show_output() -> &'static str {
    "LoadState=loaded\nActiveState=active\nSubState=running\nResult=success\nExecMainCode=0\nExecMainStatus=0\nMainPID=1234\nControlPID=0\nInvocationID=abcdef\nNRestarts=0\nRestart=on-failure\nSuccessExitStatus=143\nActiveEnterTimestampMonotonic=111\nInactiveEnterTimestampMonotonic=0\nStartLimitBurst=5\nStartLimitIntervalUSec=10000000\n"
}

#[test]
fn exhausted_budget_reports_missing_without_running() {
    // No time remains: nothing starts, nothing runs.
    let runner = FakeRunner::new(vec![]);
    let clock = ManualClock::new(Duration::ZERO);
    let t0 = clock.now();
    let out = run_diagnostic(
        &runner,
        &clock,
        vec!["true".to_string()],
        "probe",
        t0,
        Duration::from_secs(5),
        diag::RENDER_CAP,
    );
    assert!(out.contains("MISSING"), "{out}");
    assert!(runner.argv_seen().is_empty());
}

#[test]
fn small_remaining_budget_still_probes_with_clamped_exec() {
    // 3s remain of a 5s allowance: the probe runs with 3s
    // and the timeout label names the actual grant.
    let runner = FakeRunner::new(vec![timeout_with("p-out", "p-err")]);
    let clock = ManualClock::new(Duration::ZERO);
    let t0 = clock.now();
    let out = run_diagnostic(
        &runner,
        &clock,
        vec!["podman".to_string()],
        "probe",
        t0 + Duration::from_secs(3),
        Duration::from_secs(5),
        diag::RENDER_CAP,
    );
    assert!(out.contains("timed out after 3s"), "{out}");
    assert!(out.contains("p-out"), "{out}");
    assert!(out.contains("p-err"), "{out}");
}

#[test]
fn budget_windows_fit_inside_the_phase() {
    let runner = FakeRunner::new(vec![ok(0, "abc123 2026-10-26 00:01:00 +0000 UTC\n", "")]);
    let clock = ManualClock::new(Duration::from_millis(100));
    let t0 = clock.now();
    let phase = t0 + Duration::from_secs(60);
    let out = run_diagnostic(
        &runner,
        &clock,
        vec!["podman".to_string()],
        "probe",
        phase,
        Duration::from_secs(5),
        diag::RENDER_CAP,
    );
    assert!(out.contains("Exited(0)"), "{out}");
    let deadline = runner.recorded.borrow()[0].1;
    // Deadline carries the full 5s execution allowance
    // plus the 20s cleanup reserve, inside the phase.
    assert!(deadline <= phase, "{deadline:?}");
    assert!(deadline >= t0 + Duration::from_secs(25), "{deadline:?}");
}

#[test]
fn baseline_runs_before_mutation_with_real_unit() {
    // Order pin: baseline probes, then the surgery
    // command, then the landing poll — against the real
    // `.service` unit with explicit properties.
    let runner = FakeRunner::new(vec![
        // Baseline: inspect (same), exists, show.
        ok(0, "abc123 2026-10-26 00:00:00 +0000 UTC\n", ""),
        ok(0, "", ""),
        ok(0, show_output(), ""),
        // Surgery.
        ok(0, "", ""),
        // Wait poll: changed identity lands.
        ok(0, "abc123 2026-10-26 00:01:00 +0000 UTC\n", ""),
        ok(0, "", ""),
    ]);
    let clock = ManualClock::new(Duration::from_millis(100));
    let argv = vec!["podman".to_string(), "restart".to_string(), "c".to_string()];
    let (surgery, maybe_evidence) =
        run_surgery_cycle(&runner, &clock, "c", BOUND_ID, BOUND_STARTED, &argv);
    assert!(surgery.expect("surgery runs").status.success());
    let evidence = maybe_evidence.expect("landing evidence on success");
    assert!(
        evidence.history_report.contains("changed=1"),
        "{}",
        evidence.history_report
    );
    let seen = runner.argv_seen();
    assert_eq!(seen[0][0], "podman");
    assert!(seen[0].contains(&"inspect".to_string()), "{seen:?}");
    assert!(seen[1].contains(&"exists".to_string()), "{seen:?}");
    assert!(
        seen[2][0].contains("systemctl") || seen[2][0] == "systemctl",
        "{seen:?}"
    );
    assert!(seen[2].join(" ").contains("c.service"), "{seen:?}");
    assert!(seen[2].join(" ").contains("LoadState"), "{seen:?}");
    assert_eq!(seen[3], argv, "{seen:?}");
    for prop in [
        "LoadState",
        "ActiveState",
        "SubState",
        "Result",
        "ExecMainCode",
        "ExecMainStatus",
        "MainPID",
        "ControlPID",
        "InvocationID",
        "NRestarts",
        "Restart",
        "SuccessExitStatus",
        "ActiveEnterTimestampMonotonic",
        "InactiveEnterTimestampMonotonic",
        "StartLimitBurst",
        "StartLimitIntervalUSec",
    ] {
        assert!(
            evidence.baseline_report.contains(prop),
            "missing {prop}: {}",
            evidence.baseline_report
        );
    }
}

#[test]
fn changed_identity_with_small_remaining_budget_lands() {
    // Under the old full-window rule this poll could not
    // run (20s remain < 25s window) and the landing would
    // be missed. The composed budget grants the full 5s
    // execution with cleanup accounted past the phase.
    let runner = FakeRunner::new(vec![
        same_inspect(),
        ok(0, "", ""),
        ok(0, show_output(), ""),
        ok(0, "", ""),
        same_inspect(),
        ok(0, "", ""),
        changed_inspect(),
        ok(0, "", ""),
    ]);
    // Jump the clock so the second poll starts with 20s
    // left of W: index counts now() calls (baseline 4,
    // surgery 1, t0 1, iter1 check+poll 6).
    let mut steps = vec![Duration::ZERO; 12];
    steps.extend_from_slice(&[Duration::from_secs(40), Duration::ZERO]);
    let clock = ManualClock::scripted(steps, Duration::ZERO);
    let argv = vec!["podman".to_string(), "restart".to_string(), "c".to_string()];
    let (surgery, maybe_evidence) =
        run_surgery_cycle(&runner, &clock, "c", BOUND_ID, BOUND_STARTED, &argv);
    assert!(surgery.expect("surgery runs").status.success());
    let evidence = maybe_evidence.expect("landing evidence on success");
    // Landing happened (not a timeout): two polls ran, no
    // MISSING was fabricated for the narrowed window.
    assert_eq!(runner.argv_seen().len(), 8, "{evidence:?}");
    assert!(
        evidence.history_report.contains("changed=1"),
        "{}",
        evidence.history_report
    );
    assert!(
        !evidence.history_report.contains("MISSING"),
        "{}",
        evidence.history_report
    );
}

#[test]
fn changed_identity_past_deadline_is_rejected() {
    // The changed reading arrives, but the acceptance
    // check runs past W: no landing, late_rejected set,
    // timeout verdict with the post-mortem.
    let runner = FakeRunner::new(vec![
        same_inspect(),
        ok(0, "", ""),
        ok(0, show_output(), ""),
        ok(0, "", ""),
        same_inspect(),
        ok(0, "", ""),
        changed_inspect(),
        ok(0, "", ""),
        ok(0, show_output(), ""),
        ok(0, "c-stub\n", ""),
        ok(0, "journal\n", ""),
    ]);
    // Jump the clock at the exists-end of poll 2 so the
    // acceptance check fires past the deadline. now()
    // calls: baseline 4, surgery 1, t0 1, iter1 6, iter2
    // check 1, poll2 inspect/end/exists/end 4 = 17, then
    // the recheck observes the jumped clock.
    let mut steps = vec![Duration::ZERO; 16];
    steps.extend_from_slice(&[Duration::from_secs(70), Duration::ZERO]);
    let clock = ManualClock::scripted(steps, Duration::ZERO);
    let argv = vec!["podman".to_string(), "restart".to_string(), "c".to_string()];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_surgery_cycle(&runner, &clock, "c", BOUND_ID, BOUND_STARTED, &argv);
    }));
    let err = result.expect_err("late change must time out, not land");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .expect("panic message");
    assert!(msg.contains("surgery never landed"), "{msg}");
    assert!(msg.contains("late_rejected=1"), "{msg}");
    assert!(msg.contains("past the deadline"), "{msg}");
    assert!(msg.contains("LoadState=loaded"), "{msg}");
}

#[test]
fn failed_mutation_reports_original_error_without_waiting() {
    // Non-zero surgery exit: no landing polls run; the
    // original failure is returned as-is for the caller
    // to assert, never absorbed into a timeout.
    let runner = FakeRunner::new(vec![
        same_inspect(),
        ok(0, "", ""),
        ok(0, show_output(), ""),
        ok(1, "", "restart failed: unit gone"),
    ]);
    let clock = ManualClock::new(Duration::ZERO);
    let t0 = clock.now();
    let argv = vec!["podman".to_string(), "restart".to_string(), "c".to_string()];
    let (surgery, evidence) =
        run_surgery_cycle(&runner, &clock, "c", BOUND_ID, BOUND_STARTED, &argv);
    let out = surgery.expect("runner error is Ok");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unit gone"));
    assert!(evidence.is_none());
    // Exactly baseline + surgery ran; no landing polls.
    assert_eq!(runner.argv_seen().len(), 4);
    // Full 60s execution grant plus the cleanup reserve.
    assert_eq!(runner.recorded.borrow()[3].1, t0 + Duration::from_secs(80));
}

#[test]
fn spawn_failed_mutation_reports_without_waiting() {
    // Spawn failure: same contract as a non-zero exit.
    let runner = FakeRunner::new(vec![
        same_inspect(),
        ok(0, "", ""),
        ok(0, show_output(), ""),
        Err(BoundedRunError::Spawn("no daemon".to_string())),
    ]);
    let clock = ManualClock::new(Duration::ZERO);
    let argv = vec!["podman".to_string(), "restart".to_string(), "c".to_string()];
    let (surgery, evidence) =
        run_surgery_cycle(&runner, &clock, "c", BOUND_ID, BOUND_STARTED, &argv);
    match surgery {
        Err(BoundedRunError::Spawn(detail)) => {
            assert!(detail.contains("no daemon"), "{detail}")
        }
        other => panic!("expected spawn failure, got {other:?}"),
    }
    assert!(evidence.is_none());
    assert_eq!(runner.argv_seen().len(), 4);
}

#[test]
fn timeout_report_leads_with_original_verdict_over_healthy_postmortem() {
    // Polls fail unqueryably; the post-mortem then observes
    // a healthy unit. The report still leads with the
    // latched timeout verdict — later healthy observations
    // never replace it.
    let runner = FakeRunner::new(vec![
        timeout_with("partial-out", "partial-err"),
        ok(0, "", ""),
        ok(0, show_output(), ""),
        ok(0, "", ""),
        timeout_with("partial-out", "partial-err"),
        ok(125, "", ""),
        timeout_with("partial-out", "partial-err"),
        ok(125, "", ""),
        ok(0, "ActiveState=active\n", ""),
        ok(0, "c-stub abc123 running\n", ""),
        ok(0, "journal-ok\n", ""),
    ]);
    let clock = ManualClock::new(Duration::from_secs(5));
    let argv = vec!["podman".to_string(), "restart".to_string(), "c".to_string()];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_surgery_cycle(&runner, &clock, "c", BOUND_ID, BOUND_STARTED, &argv);
    }));
    let err = result.expect_err("unqueryable polls must time out");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .expect("panic message");
    let verdict = msg.find("surgery never landed").expect("verdict first");
    let section = msg
        .find("not successfully queried")
        .expect("unqueryable section");
    let baseline_at = msg.find("baseline: ").expect("baseline");
    let history_at = msg.find("history: ").expect("history");
    let postmortem_at = msg.find("postmortem: ").expect("postmortem");
    assert!(
        verdict < section
            && section < baseline_at
            && baseline_at < history_at
            && history_at < postmortem_at,
        "{msg}"
    );
    assert!(msg.contains("partial-out"), "{msg}");
    assert!(msg.contains("ActiveState=active"), "{msg}");
    assert!(msg.contains("timed out after"), "{msg}");
}

#[test]
fn timeout_partials_and_missing_are_labeled() {
    let runner = FakeRunner::new(vec![
        timeout_with("partial-out", "partial-err"),
        ok(125, "", "no such object: c"),
    ]);
    let clock = ManualClock::new(Duration::from_millis(100));
    let t0 = clock.now();
    let wait_deadline = t0 + Duration::from_secs(60);
    let mut history = diag::PollHistory::default();
    let landed = poll_landing(
        &runner,
        &clock,
        "c",
        BOUND_ID,
        BOUND_STARTED,
        t0,
        wait_deadline,
        0,
        &mut history,
    );
    assert!(landed.is_none());
    let report = diag::report_history(&history);
    assert!(report.contains("unqueryable=1"), "{report}");
    assert!(report.contains("partial-out"), "{report}");
    assert!(report.contains("partial-err"), "{report}");
    assert!(report.contains("timed out after 5s"), "{report}");
    assert!(report.contains("no such object"), "{report}");
}

#[test]
fn guard_disarms_cleanly() {
    let mut guard = EvidenceGuard::armed("evidence".to_string());
    assert!(guard.is_armed());
    guard.disarm();
    assert!(!guard.is_armed());
}

#[test]
fn probe_reserve_matches_bounded_allowances() {
    // The composition stays honest only while the
    // reserve equals the harness cleanup allowances.
    assert_eq!(PROBE_RESERVE, REAP_ALLOWANCE + POST_EXIT_DRAIN_TIMEOUT);
}
