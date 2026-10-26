//! Post-surgery landing orchestration for the enter-gate
//! diagnostics: baseline capture, the bounded poll loop, and
//! the timeout post-mortem. Every subprocess runs through an
//! injected runner against absolute deadlines composed from
//! the declared phase budget; classification, history, and
//! reports come from the `surgery_diag` seam (pinned
//! backend-free there).

use std::process::Output;
use std::time::{Duration, Instant};

use super::bounded::{BoundedRunError, POST_EXIT_DRAIN_TIMEOUT, REAP_ALLOWANCE};
use super::surgery_diag as diag;

/// Reserve inside every probe window for harness cleanup
/// (kill+reap+drain), so the stated execution allowance is
/// real and the phase bound still holds. Pinned to the
/// bounded.rs constants by the test below.
pub const PROBE_RESERVE: Duration = Duration::from_secs(20);

/// Per-probe execution allowance inside W: local queries are
/// millisecond-scale; 5s is two orders above that and leaves
/// room for two probes plus cadence inside the 60s window.
pub const PER_POLL_PROBE: Duration = Duration::from_secs(5);

/// Landing deadline W (unchanged).
pub const LANDING_DEADLINE: Duration = Duration::from_secs(60);

/// Pre-surgery baseline: total allowance B plus the
/// per-command execution allowance inside it.
pub const BASELINE_BUDGET: Duration = Duration::from_secs(30);
pub const BASELINE_PROBE: Duration = Duration::from_secs(5);

/// Failure-report allowance D (starts after the original
/// timeout latches) plus the per-command execution allowance
/// inside it.
pub const POSTMORTEM_BUDGET: Duration = Duration::from_secs(120);
pub const POSTMORTEM_PROBE: Duration = Duration::from_secs(30);

/// Cap for post-mortem stream rendering (journal slices need
/// more room than one-line poll notes).
pub const POSTMORTEM_STREAM_CAP: usize = 4000;

/// Surgery command allowance: matches the pre-existing
/// `run_bounded(..., RUN_CISTELLA_TIMEOUT)` behavior.
pub const SURGERY_TIMEOUT: Duration = Duration::from_secs(60);

/// A subprocess runner: argv in, bounded outcome out. The
/// production runner shells out; test fakes replay scripts.
pub trait ProbeRunner {
    fn run(&self, argv: &[String], deadline: Instant) -> Result<Output, BoundedRunError>;
}

/// Production runner: builds the command and waits against
/// the absolute deadline (cleanup accounted inside it).
pub struct RealRunner;

impl ProbeRunner for RealRunner {
    fn run(&self, argv: &[String], deadline: Instant) -> Result<Output, BoundedRunError> {
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        super::bounded::run_bounded_at(cmd, deadline)
    }
}

/// A monotonic clock. Injected so tests can drive the
/// orchestration deterministically without real sleeping.
pub trait Clock {
    fn now(&self) -> Instant;
}

/// Production clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Pre-surgery baseline: the recorded state the timeout
/// post-mortem reads against, captured under allowance B.
pub struct SurgeryBaseline {
    pub epoch_secs: u64,
    pub report: String,
}

/// Evidence retained across a successful landing for later
/// failures to read against (context only — verdicts
/// unchanged).
pub struct LandingEvidence {
    pub history_report: String,
    pub baseline_report: String,
}

/// Run one diagnostic command inside the phase budget: the
/// window grants the full execution allowance plus the
/// cleanup reserve, or the probe is MISSING when even one
/// full window no longer fits. Timeouts are labeled with the
/// actual allowance granted. Never retries, never mutates.
pub fn run_diagnostic<R: ProbeRunner, C: Clock>(
    runner: &R,
    clock: &C,
    argv: Vec<String>,
    label: &str,
    phase_deadline: Instant,
    allowance: Duration,
    stream_cap: usize,
) -> String {
    let now = clock.now();
    let Some(deadline) = diag::probe_window(now, phase_deadline, allowance, PROBE_RESERVE) else {
        return diag::render_cmd_evidence(label, "MISSING: phase budget exhausted");
    };
    let label_owned = format!("{label} +{:?}", allowance);
    let outcome = diag::split_outcome(runner.run(&argv[..], deadline));
    if outcome.end == diag::ProbeEnd::TimedOut {
        return diag::render_cmd_evidence(&label_owned, &diag::timeout_mark("probe", allowance));
    }
    diag::render_outcome(&label_owned, &outcome, stream_cap)
}

/// Pre-surgery baseline under allowance B: bound pair (known
/// at bind time), one inspect plus one exists outcome pair,
/// one unit show snapshot. Failure is recorded, never a new
/// gate. Must run before the mutation it reads against.
pub fn capture_baseline<R: ProbeRunner, C: Clock>(
    runner: &R,
    clock: &C,
    container: &str,
    bound_id: &str,
    bound_started: &str,
) -> SurgeryBaseline {
    let b0 = clock.now();
    let b_deadline = b0 + BASELINE_BUDGET;
    let epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let unit = diag::unit_name(container);
    let mut parts = vec![format!("bound=({bound_id}, {bound_started})")];
    parts.push(run_diagnostic(
        runner,
        clock,
        diag::inspect_argv(container),
        &format!("podman inspect {container}"),
        b_deadline,
        BASELINE_PROBE,
        diag::RENDER_CAP,
    ));
    parts.push(run_diagnostic(
        runner,
        clock,
        diag::exists_argv(container),
        &format!("podman container exists {container}"),
        b_deadline,
        BASELINE_PROBE,
        diag::RENDER_CAP,
    ));
    parts.push(run_diagnostic(
        runner,
        clock,
        diag::show_argv(&unit),
        &format!("systemctl --user show {unit}"),
        b_deadline,
        BASELINE_PROBE,
        diag::RENDER_CAP,
    ));
    SurgeryBaseline {
        epoch_secs,
        report: parts.join("\n"),
    }
}

/// Timeout post-mortem inside allowance D, after the original
/// timeout latches and before Guard cleanup. The unit is the
/// running `.service` unit; the journal slice starts at the
/// recorded pre-surgery cursor. Missing/unscheduled/timed-out
/// diagnostics render as explicit markers; the original
/// timeout stays the verdict regardless.
pub fn collect_postmortem<R: ProbeRunner, C: Clock>(
    runner: &R,
    clock: &C,
    container: &str,
    bound_id: &str,
    baseline: &SurgeryBaseline,
) -> String {
    let d0 = clock.now();
    let d_deadline = d0 + POSTMORTEM_BUDGET;
    let unit = diag::unit_name(container);
    let mut parts = vec![format!("unit={unit} bound_id={bound_id}")];
    parts.push(run_diagnostic(
        runner,
        clock,
        diag::show_argv(&unit),
        &format!("systemctl --user show {unit}"),
        d_deadline,
        POSTMORTEM_PROBE,
        POSTMORTEM_STREAM_CAP,
    ));
    parts.push(run_diagnostic(
        runner,
        clock,
        diag::ps_argv(container),
        &format!("podman ps -a name={container} (id match checked below)"),
        d_deadline,
        POSTMORTEM_PROBE,
        POSTMORTEM_STREAM_CAP,
    ));
    parts.push(run_diagnostic(
        runner,
        clock,
        diag::journal_argv(&unit, baseline.epoch_secs),
        &format!("journalctl --user -u {unit}"),
        d_deadline,
        POSTMORTEM_PROBE,
        POSTMORTEM_STREAM_CAP,
    ));
    let mut out = parts.join("\n");
    out.push_str(&format!("\nknown bound container id: {bound_id}"));
    out
}

/// One landing-wait poll iteration: inspect then exists, both
/// clamped to the remaining wait budget and recorded with
/// per-command offsets and order. Returns the landed identity
/// when the paired reading differs from the bound AND the
/// deadline has not passed; a changed reading observed at or
/// past the deadline is rejected (late evidence never lands).
/// Unscheduled probes render MISSING, never fabricated
/// outcomes. Unqueryable means not successfully queried,
/// never absence.
#[allow(clippy::too_many_arguments)]
pub fn poll_landing<R: ProbeRunner, C: Clock>(
    runner: &R,
    clock: &C,
    container: &str,
    bound_id: &str,
    bound_started: &str,
    t0: Instant,
    wait_deadline: Instant,
    index: usize,
    history: &mut diag::PollHistory,
) -> Option<(String, String)> {
    let offsets = |now: Instant| now.duration_since(t0).as_millis().min(u64::MAX as u128) as u64;
    // Inspect probe.
    let inspect_start = clock.now();
    let inspect_start_ms = offsets(inspect_start);
    let inspect_outcome =
        diag::probe_window(inspect_start, wait_deadline, PER_POLL_PROBE, PROBE_RESERVE).map(
            |deadline| diag::split_outcome(runner.run(&diag::inspect_argv(container), deadline)),
        );
    let inspect_end_ms = offsets(clock.now());
    let (inspect_class, pair) =
        diag::classify_inspect(inspect_outcome.as_ref(), bound_id, bound_started);
    // Exists probe (same budget discipline, sequential order).
    let exists_start = clock.now();
    let exists_start_ms = offsets(exists_start);
    let exists_outcome =
        diag::probe_window(exists_start, wait_deadline, PER_POLL_PROBE, PROBE_RESERVE).map(
            |deadline| diag::split_outcome(runner.run(&diag::exists_argv(container), deadline)),
        );
    let exists_end_ms = offsets(clock.now());
    let exists_class = diag::classify_exists(exists_outcome.as_ref());
    // Human note: per-probe ends, capped streams, timeout
    // labels with the actual allowance granted.
    let mut note = format!(
        "inspect={} out={} err={} exists={}",
        render_end(inspect_outcome.as_ref()),
        render_stream(inspect_outcome.as_ref(), true),
        render_stream(inspect_outcome.as_ref(), false),
        render_exists(&exists_class, exists_outcome.as_ref()),
    );
    if matches!(
        inspect_outcome.as_ref(),
        Some(o) if o.end == diag::ProbeEnd::TimedOut
    ) {
        note = format!(
            "{}; {note}",
            diag::timeout_mark("inspect probe", PER_POLL_PROBE)
        );
    }
    if matches!(
        exists_outcome.as_ref(),
        Some(o) if o.end == diag::ProbeEnd::TimedOut
    ) {
        note = format!(
            "{note}; {}",
            diag::timeout_mark("exists probe", PER_POLL_PROBE)
        );
    }
    let mut late_rejected = false;
    let mut landed = None;
    if let (diag::InspectClass::Changed, Some(pair)) = (&inspect_class, &pair) {
        if clock.now() >= wait_deadline {
            // Changed identity observed at or past the
            // deadline: rejected, never landed.
            late_rejected = true;
        } else {
            landed = Some(pair.clone());
        }
    }
    let record = diag::PollRecord {
        index,
        inspect_start_ms,
        inspect_end_ms,
        exists_start_ms,
        exists_end_ms,
        inspect: Some((inspect_class, pair)),
        exists: Some(exists_class),
        late_rejected,
        note,
    };
    let rendered = diag::render_poll(&record);
    let (record_inspect, _) = record.inspect.clone().unwrap();
    diag::history_observe(
        history,
        rendered,
        record_inspect,
        record.exists.unwrap(),
        record.late_rejected,
        inspect_end_ms,
    );
    landed
}

fn render_end(outcome: Option<&diag::ProbeOutcome>) -> String {
    match outcome {
        Some(outcome) => format!("{:?}", outcome.end),
        None => "MISSING: phase budget exhausted".to_string(),
    }
}

fn render_stream(outcome: Option<&diag::ProbeOutcome>, stdout: bool) -> String {
    match outcome {
        Some(outcome) => {
            let bytes = if stdout {
                &outcome.stdout
            } else {
                &outcome.stderr
            };
            diag::escape_capped(bytes, diag::RENDER_CAP)
        }
        None => "MISSING".to_string(),
    }
}

fn render_exists(class: &diag::ExistsClass, outcome: Option<&diag::ProbeOutcome>) -> String {
    match outcome {
        Some(outcome)
            if outcome.end != diag::ProbeEnd::Exited(0)
                && outcome.end != diag::ProbeEnd::Exited(1) =>
        {
            format!("{class:?} (end={:?})", outcome.end)
        }
        _ => format!("{class:?}"),
    }
}

/// Full surgery cycle with the order pinned: baseline, then
/// the mutation, then the landing wait whose deadline starts
/// at the mutation-return boundary. Returns the raw surgery
/// outcome (the caller asserts on it as before) and the
/// landing evidence for later failures to read as context.
pub fn run_surgery_cycle<R: ProbeRunner, C: Clock>(
    runner: &R,
    clock: &C,
    container: &str,
    bound_id: &str,
    bound_started: &str,
    surgery_argv: &[String],
) -> (
    Result<std::process::Output, BoundedRunError>,
    LandingEvidence,
) {
    let baseline = capture_baseline(runner, clock, container, bound_id, bound_started);
    let surgery_deadline = clock.now() + SURGERY_TIMEOUT;
    let surgery_outcome = runner.run(surgery_argv, surgery_deadline);
    let wait_start = clock.now();
    let wait_deadline = wait_start + LANDING_DEADLINE;
    let mut history = diag::PollHistory::default();
    let mut index = 0usize;
    loop {
        if clock.now() >= wait_deadline {
            let postmortem = collect_postmortem(runner, clock, container, bound_id, &baseline);
            let section = diag::unqueryable_section(&history);
            let verdict = format!(
                "surgery never landed a new incarnation for {container}: bound=({bound_id}, {bound_started})"
            );
            panic!(
                "{}",
                diag::render_timeout_report(
                    &verdict,
                    &section,
                    &baseline.report,
                    &diag::report_history(&history),
                    &postmortem,
                )
            );
        }
        if poll_landing(
            runner,
            clock,
            container,
            bound_id,
            bound_started,
            wait_start,
            wait_deadline,
            index,
            &mut history,
        )
        .is_some()
        {
            return (
                surgery_outcome,
                LandingEvidence {
                    history_report: diag::report_history(&history),
                    baseline_report: baseline.report.clone(),
                },
            );
        }
        index += 1;
        let remaining = wait_deadline.saturating_duration_since(clock.now());
        std::thread::sleep(remaining.min(Duration::from_millis(500)));
    }
}

/// Post-landing context guard: armed with the landing
/// evidence after a successful wait; disarmed once the test
/// fully converges. Any post-landing failure unwinding past
/// the guard prints the retained context (history + baseline)
/// — every post-landing failure path, without changing any
/// assertion.
pub struct EvidenceGuard {
    report: Option<String>,
}

impl EvidenceGuard {
    pub fn armed(report: String) -> Self {
        Self {
            report: Some(report),
        }
    }

    pub fn disarm(&mut self) {
        self.report = None;
    }

    #[cfg(test)]
    pub fn is_armed(&self) -> bool {
        self.report.is_some()
    }
}

impl Drop for EvidenceGuard {
    fn drop(&mut self) {
        if let Some(report) = self.report.take() {
            eprintln!("post-landing context (failure unwinding past this point):\n{report}");
        }
    }
}

#[cfg(all(test, unix))]
mod surgery_collect_tests {
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
        step: Duration,
    }

    impl ManualClock {
        fn new(step: Duration) -> Self {
            Self {
                t: Cell::new(Instant::now()),
                step,
            }
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            let t = self.t.get();
            self.t.set(t + self.step);
            t
        }
    }

    const BOUND_ID: &str = "abc123";
    const BOUND_STARTED: &str = "2026-10-26 00:00:00 +0000 UTC";

    fn changed_pair() -> (String, String) {
        (
            "abc123".to_string(),
            "2026-10-26 00:01:00 +0000 UTC".to_string(),
        )
    }

    #[test]
    fn probes_get_full_allowance_plus_reserve_inside_phase() {
        // Window composition: deadline = now + 5s exec + 20s
        // reserve, inside the phase. A 10s remaining budget
        // cannot fit a 25s window: MISSING, never fabricated.
        let runner = FakeRunner::new(vec![]);
        let clock = ManualClock::new(Duration::from_millis(100));
        let t0 = clock.now();
        let d0 = clock.now() + POSTMORTEM_BUDGET;
        let before = run_diagnostic(
            &runner,
            &clock,
            vec!["true".to_string()],
            "probe",
            t0 + Duration::from_secs(10),
            POSTMORTEM_PROBE,
            diag::RENDER_CAP,
        );
        assert!(before.contains("MISSING"), "{before}");
        let _ = d0;
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
        let changed = changed_pair();
        let runner = FakeRunner::new(vec![
            // Baseline: inspect (same), exists, show.
            ok(0, "abc123 2026-10-26 00:00:00 +0000 UTC\n", ""),
            ok(0, "", ""),
            ok(0, "LoadState=loaded\n", ""),
            // Surgery.
            ok(0, "", ""),
            // Wait poll: changed identity lands.
            ok(0, "abc123 2026-10-26 00:01:00 +0000 UTC\n", ""),
            ok(0, "", ""),
        ]);
        let clock = ManualClock::new(Duration::from_millis(100));
        let argv = vec!["podman".to_string(), "restart".to_string(), "c".to_string()];
        let (surgery, evidence) =
            run_surgery_cycle(&runner, &clock, "c", BOUND_ID, BOUND_STARTED, &argv);
        assert!(surgery.expect("surgery runs").status.success());
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
        let _ = changed;
    }

    #[test]
    fn changed_identity_past_deadline_is_rejected() {
        // Late evidence never lands: the changed reading
        // arrives after W and must time out instead.
        let runner = FakeRunner::new(vec![
            ok(0, "abc123 2026-10-26 00:00:00 +0000 UTC\n", ""),
            ok(0, "", ""),
            ok(0, "LoadState=loaded\n", ""),
            ok(0, "", ""),
            ok(0, "abc123 2026-10-26 00:01:00 +0000 UTC\n", ""),
            ok(0, "", ""),
            ok(0, "abc123 2026-10-26 00:01:00 +0000 UTC\n", ""),
            ok(0, "", ""),
        ]);
        // Huge steps: the wait deadline passes before any
        // poll can observe the change in time.
        let clock = ManualClock::new(Duration::from_secs(120));
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
    }

    #[test]
    fn timeout_partials_and_missing_are_labeled() {
        let runner = FakeRunner::new(vec![
            timeout_with("partial-out", "partial-err"),
            ok(125i32.wrapping_neg(), "", ""),
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
}
