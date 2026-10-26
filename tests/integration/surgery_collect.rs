//! Post-surgery landing orchestration for the enter-gate
//! diagnostics: baseline capture, the bounded poll loop, and
//! the timeout post-mortem. All subprocesses run through the
//! phase budgets; classification, history, and reports come
//! from the `surgery_diag` seam (pinned backend-free there).

use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::bounded::run_bounded_at;
use super::surgery_diag::*;

/// Allowance for one poll probe inside the landing wait.
/// Single local queries are millisecond-scale; 5s is two
/// orders above that and two below the 60s deadline, and a
/// probe that exceeds it is marked timeout, never fatal.
/// Recorded rationale: not derived from retained artifacts
/// (which carry no per-poll timings).
const PER_POLL_PROBE_ALLOWANCE: Duration = Duration::from_secs(5);

/// Landing deadline W (unchanged): the replacement must be
/// observed within 60s of the surgery command returning.
const LANDING_DEADLINE: Duration = Duration::from_secs(60);

/// Pre-surgery baseline allowance B, per command and total:
/// three fast local queries on the success path, so the
/// allowance only bites on a wedged seat; baseline failure is
/// recorded, never a new gate.
const BASELINE_PROBE_ALLOWANCE: Duration = Duration::from_secs(10);
const BASELINE_BUDGET: Duration = Duration::from_secs(30);

/// Failure-report allowance D: the timeout post-mortem runs
/// only after the original timeout latches, inside its own
/// budget — never charged to the poll budget it outlived.
/// Post-mortem commands each get at most 30s of the 120s.
const POSTMORTEM_BUDGET: Duration = Duration::from_secs(120);
const POSTMORTEM_PROBE_ALLOWANCE: Duration = Duration::from_secs(30);

/// Cap for post-mortem stream rendering (journal slices need
/// more room than one-line poll notes).
const POSTMORTEM_STREAM_CAP: usize = 4000;

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

/// Run one diagnostic command inside the phase budget.
/// Unscheduled commands (no budget left) and failed commands
/// render as explicit missing/timeout markers, never
/// silence. Timeouts are labeled as timeouts, never counted
/// as backend outcomes.
pub fn run_diagnostic(
    cmd: Command,
    label: String,
    phase_deadline: Instant,
    allowance: Duration,
    stream_cap: usize,
) -> String {
    let Some(allow) = clamp_probe_allowance(Instant::now(), phase_deadline, allowance) else {
        return render_cmd_evidence(&label, "MISSING: phase budget exhausted");
    };
    let (end, stdout, stderr) = split_outcome(run_bounded_at(cmd, Instant::now() + allow));
    if end == ProbeEnd::TimedOut {
        return render_cmd_evidence(&label, &timeout_mark(&label, allow));
    }
    render_cmd_evidence(
        &label,
        &format!(
            "{end:?} stdout={} stderr={}",
            escape_capped(&stdout, stream_cap),
            escape_capped(&stderr, stream_cap),
        ),
    )
}

/// Pre-surgery baseline under allowance B: bound pair (known
/// at bind time), one inspect plus one exists outcome pair,
/// one unit show snapshot. Failure is recorded without
/// changing test behavior — setup assumptions stay visible.
pub fn capture_baseline(container: &str, bound_id: &str, bound_started: &str) -> SurgeryBaseline {
    let b0 = Instant::now();
    let b_deadline = b0 + BASELINE_BUDGET;
    let epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let unit = format!("{container}.container");
    let mut parts = vec![format!("bound=({bound_id}, {bound_started})")];
    let mut inspect_cmd = Command::new("podman");
    inspect_cmd.args([
        "inspect",
        "--format",
        "{{.Id}} {{.State.StartedAt}}",
        container,
    ]);
    parts.push(run_diagnostic(
        inspect_cmd,
        format!("podman inspect {container}"),
        b_deadline,
        BASELINE_PROBE_ALLOWANCE,
        RENDER_CAP,
    ));
    let mut exists_cmd = Command::new("podman");
    exists_cmd.args(["container", "exists", container]);
    parts.push(run_diagnostic(
        exists_cmd,
        format!("podman container exists {container}"),
        b_deadline,
        BASELINE_PROBE_ALLOWANCE,
        RENDER_CAP,
    ));
    let mut show_cmd = Command::new("systemctl");
    show_cmd.args(["--user", "show", &unit]);
    parts.push(run_diagnostic(
        show_cmd,
        format!("systemctl --user show {unit}"),
        b_deadline,
        BASELINE_PROBE_ALLOWANCE,
        RENDER_CAP,
    ));
    SurgeryBaseline {
        epoch_secs,
        report: parts.join("\n"),
    }
}

/// One heavy post-mortem inside allowance D, before Guard
/// cleanup. Missing/unscheduled/timed-out diagnostics render
/// as explicit markers; the original timeout stays the
/// verdict regardless.
pub fn collect_postmortem(container: &str, baseline: &SurgeryBaseline) -> String {
    let d0 = Instant::now();
    let d_deadline = d0 + POSTMORTEM_BUDGET;
    let unit = format!("{container}.container");
    let mut parts = Vec::new();
    let mut show_cmd = Command::new("systemctl");
    show_cmd.args(["--user", "show", &unit]);
    parts.push(run_diagnostic(
        show_cmd,
        format!("systemctl --user show {unit}"),
        d_deadline,
        POSTMORTEM_PROBE_ALLOWANCE,
        POSTMORTEM_STREAM_CAP,
    ));
    let mut ps_cmd = Command::new("podman");
    ps_cmd.args([
        "ps",
        "-a",
        "--filter",
        &format!("name={container}"),
        "--format",
        "{{.ID}} {{.Names}} {{.State}}",
    ]);
    parts.push(run_diagnostic(
        ps_cmd,
        format!("podman ps -a name={container}"),
        d_deadline,
        POSTMORTEM_PROBE_ALLOWANCE,
        POSTMORTEM_STREAM_CAP,
    ));
    let mut journal_cmd = Command::new("journalctl");
    journal_cmd.args([
        "--user",
        "-u",
        &unit,
        "--since",
        &format!("@{}", baseline.epoch_secs),
        "--no-pager",
        "-n",
        "100",
    ]);
    parts.push(run_diagnostic(
        journal_cmd,
        format!("journalctl --user -u {unit}"),
        d_deadline,
        POSTMORTEM_PROBE_ALLOWANCE,
        POSTMORTEM_STREAM_CAP,
    ));
    parts.join("\n")
}

/// One landing-wait poll iteration: inspect then exists, both
/// clamped to the remaining wait budget and recorded with
/// per-command offsets and order. Returns the landed
/// identity when the paired reading differs from the bound;
/// every other outcome is recorded history. Unqueryable
/// means not successfully queried, never absence.
pub fn poll_landing(
    container: &str,
    bound_id: &str,
    bound_started: &str,
    t0: Instant,
    wait_deadline: Instant,
    index: usize,
    history: &mut PollHistory,
) -> Option<(String, String)> {
    let mut inspect_cmd = Command::new("podman");
    inspect_cmd.args([
        "inspect",
        "--format",
        "{{.Id}} {{.State.StartedAt}}",
        container,
    ]);
    let inspect_start_ms = t0.elapsed().as_millis() as u64;
    let inspect_outcome = run_bounded_at(
        inspect_cmd,
        Instant::now()
            + clamp_probe_allowance(Instant::now(), wait_deadline, PER_POLL_PROBE_ALLOWANCE)?,
    );
    let inspect_end_ms = t0.elapsed().as_millis() as u64;
    let (inspect_end, inspect_out, inspect_err) = split_outcome(inspect_outcome);
    let (inspect_class, pair) =
        classify_inspect(inspect_end, &inspect_out, bound_id, bound_started);
    let mut exists_cmd = Command::new("podman");
    exists_cmd.args(["container", "exists", container]);
    let exists_start_ms = t0.elapsed().as_millis() as u64;
    let exists_class =
        match clamp_probe_allowance(Instant::now(), wait_deadline, PER_POLL_PROBE_ALLOWANCE) {
            Some(allow) => {
                let (end, _, _) = split_outcome(run_bounded_at(exists_cmd, Instant::now() + allow));
                classify_exists(end)
            }
            None => ExistsClass::QueryFailure,
        };
    let exists_end_ms = t0.elapsed().as_millis() as u64;
    let anomaly = !matches!(
        inspect_class,
        InspectClass::CleanSame | InspectClass::Changed
    ) || !matches!(exists_class, ExistsClass::Found);
    let mut note = format!(
        "inspect={inspect_end:?} out={} err={} exists={exists_class:?}",
        escape_capped(&inspect_out, RENDER_CAP),
        escape_capped(&inspect_err, RENDER_CAP),
    );
    if inspect_end == ProbeEnd::TimedOut {
        note = format!(
            "{}; {note}",
            timeout_mark("inspect probe", PER_POLL_PROBE_ALLOWANCE)
        );
    }
    let record = PollRecord {
        index,
        inspect_start_ms,
        inspect_end_ms,
        exists_start_ms,
        exists_end_ms,
        inspect_first: true,
        inspect: inspect_class.clone(),
        exists: exists_class,
        note,
    };
    let rendered = render_poll(&record);
    history_push(history, rendered, anomaly);
    history_tally(history, &record.inspect, &record.exists);
    if matches!(record.inspect, InspectClass::Changed) {
        return pair;
    }
    None
}

/// Waits for a post-surgery incarnation to land as one
/// coherent observation: a single paired reading differing
/// from the bound. Transient uninspectability reads as
/// not-yet, never as absence; the bound fails loud with the
/// bound pair, the conditional unqueryable section, the
/// pre-surgery baseline, the bounded poll history, and the
/// timeout post-mortem. Landing is the premise — the product
/// gate renders the verdict.
pub fn wait_landed_incarnation(
    container: &str,
    bound_id: &str,
    bound_started: &str,
    baseline: &SurgeryBaseline,
) -> LandingEvidence {
    let t0 = Instant::now();
    let wait_deadline = t0 + LANDING_DEADLINE;
    let mut history = PollHistory::default();
    let mut index = 0usize;
    loop {
        if Instant::now() >= wait_deadline {
            let postmortem = collect_postmortem(container, baseline);
            let section = unqueryable_section(&history);
            panic!(
                "surgery never landed a new incarnation for {container}: bound=({bound_id}, {bound_started}), {section}\nbaseline: {}\nhistory: {}\npostmortem: {postmortem}",
                baseline.report,
                report_history(&history),
            );
        }
        if poll_landing(
            container,
            bound_id,
            bound_started,
            t0,
            wait_deadline,
            index,
            &mut history,
        )
        .is_some()
        {
            return LandingEvidence {
                history_report: report_history(&history),
                baseline_report: baseline.report.clone(),
            };
        }
        index += 1;
        let remaining = wait_deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(Duration::from_millis(500)));
    }
}
