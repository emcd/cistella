//! Backend-free diagnostic record core for the enter-gate surgery
//! paths. Every function here is pure over its inputs: the test
//! body runs the subprocesses and hands the outcomes in, while
//! classification, history caps, report rendering, and budget
//! arithmetic never touch a backend. The `#[cfg(test)]` pins
//! below invoke this same implementation, so the taxonomy
//! cannot drift from the collector (no duplicated model).

use std::collections::VecDeque;
use std::process::Output;
use std::time::{Duration, Instant};

use super::bounded::BoundedRunError;

/// Per-stream render cap: keeps one-line notes log-shaped.
pub const RENDER_CAP: usize = 160;

/// Records retained at each history end (first N and last N).
pub const HISTORY_KEEP_EACH_END: usize = 3;

/// How a single probe command ended. Covers both exit codes and
/// harness-level failures (spawn/timeout/drain), so a verdict
/// can never mistake a failed probe for a backend reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeEnd {
    Exited(i32),
    Signaled,
    SpawnFailed,
    TimedOut,
    WaitFailed,
    Overflowed,
}

/// A bounded probe outcome: end state plus capped raw streams.
/// Partials from timeouts are kept; nothing is inferred.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub end: ProbeEnd,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Split a bounded-run outcome into its end state plus owned
/// stdout/stderr bytes (partials on timeout; empty when the
/// command never produced output). Pure.
#[must_use]
pub fn split_outcome(result: Result<Output, BoundedRunError>) -> ProbeOutcome {
    match result {
        Ok(out) => {
            let end = match out.status.code() {
                Some(code) => ProbeEnd::Exited(code),
                None => ProbeEnd::Signaled,
            };
            ProbeOutcome {
                end,
                stdout: out.stdout,
                stderr: out.stderr,
            }
        }
        Err(BoundedRunError::Timeout {
            partial_stdout,
            partial_stderr,
            ..
        }) => ProbeOutcome {
            end: ProbeEnd::TimedOut,
            stdout: partial_stdout.into_bytes(),
            stderr: partial_stderr.into_bytes(),
        },
        Err(BoundedRunError::Spawn(_)) => ProbeOutcome {
            end: ProbeEnd::SpawnFailed,
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
        Err(BoundedRunError::DrainTimeout {
            partial_stdout,
            partial_stderr,
        }) => ProbeOutcome {
            end: ProbeEnd::TimedOut,
            stdout: partial_stdout.into_bytes(),
            stderr: partial_stderr.into_bytes(),
        },
        Err(BoundedRunError::Wait { .. }) => ProbeOutcome {
            end: ProbeEnd::WaitFailed,
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
        Err(BoundedRunError::Overflow { .. }) => ProbeOutcome {
            end: ProbeEnd::Overflowed,
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
        Err(BoundedRunError::Capture { .. }) => ProbeOutcome {
            end: ProbeEnd::WaitFailed,
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    }
}

/// Escape a byte stream for one-line rendering: lossy text,
/// capped at `cap` kept characters, with line breaks and
/// terminal controls escaped (`\n`, `\r`, `\t`, `\\`,
/// `\x1b`, other C0 controls as `\xNN`). Truncation is
/// explicit: a `...[+N truncated]` marker names the dropped
/// count. Nothing is diagnosed from the content. Pure.
#[must_use]
pub fn escape_capped(bytes: &[u8], cap: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut kept = 0usize;
    let mut skipped = 0usize;
    for ch in text.chars() {
        if kept >= cap {
            skipped += 1;
            continue;
        }
        kept += 1;
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            '\x1b' => out.push_str("\\x1b"),
            other if other.is_control() => {
                out.push_str(&format!("\\x{:02x}", other as u32));
            }
            other => out.push(other),
        }
    }
    if skipped > 0 {
        out.push_str(&format!("...[+{skipped} truncated]"));
    }
    out
}

/// `podman container exists` classification per the CLI
/// contract: 0 found, 1 not found, anything else (including
/// 125 and signals) a storage/access query failure — never
/// absence. Unscheduled probes are `Missing`, never a query
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistsClass {
    Found,
    NotFound,
    QueryFailure,
    Missing,
}

/// Classify an exists outcome. Pure.
#[must_use]
pub fn classify_exists(outcome: Option<&ProbeOutcome>) -> ExistsClass {
    match outcome {
        None => ExistsClass::Missing,
        Some(outcome) => match outcome.end {
            ProbeEnd::Exited(0) => ExistsClass::Found,
            ProbeEnd::Exited(1) => ExistsClass::NotFound,
            _ => ExistsClass::QueryFailure,
        },
    }
}

/// Inspect outcome classification against the bound pair.
/// `Changed` and `CleanSame` require a successful parse of
/// the whole pair with the product parser (StartedAt kept
/// intact — never reconstructed from whitespace tokens).
/// Unscheduled probes are `Missing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectClass {
    CleanSame,
    Changed,
    Unparseable,
    Unqueryable,
    Missing,
}

/// Classify an inspect outcome. Pure; parses with the product
/// parser and compares whole pairs. Returns the class plus
/// the parsed identity when one exists.
#[must_use]
pub fn classify_inspect(
    outcome: Option<&ProbeOutcome>,
    bound_id: &str,
    bound_started: &str,
) -> (InspectClass, Option<(String, String)>) {
    let Some(outcome) = outcome else {
        return (InspectClass::Missing, None);
    };
    if outcome.end != ProbeEnd::Exited(0) {
        return (InspectClass::Unqueryable, None);
    }
    match cistella::attestation::parse_identity(&String::from_utf8_lossy(&outcome.stdout)) {
        Ok(identity) => {
            let pair = (identity.container_id, identity.started_at);
            if pair.0 == bound_id && pair.1 == bound_started {
                (InspectClass::CleanSame, Some(pair))
            } else {
                (InspectClass::Changed, Some(pair))
            }
        }
        Err(_) => (InspectClass::Unparseable, None),
    }
}

/// One poll iteration's diagnostic record. Missing probes are
/// `None` (unscheduled), rendered as MISSING — never folded
/// into a backend outcome.
#[derive(Debug, Clone)]
pub struct PollRecord {
    pub index: usize,
    pub inspect_start_ms: u64,
    pub inspect_end_ms: u64,
    pub exists_start_ms: u64,
    pub exists_end_ms: u64,
    pub inspect: Option<(InspectClass, Option<(String, String)>)>,
    pub exists: Option<ExistsClass>,
    pub late_rejected: bool,
    pub note: String,
}

/// Render one record as one line, including the probe order.
/// Pure.
#[must_use]
pub fn render_poll(record: &PollRecord) -> String {
    let inspect = match &record.inspect {
        Some((class, _)) => format!("{class:?}"),
        None => "Missing".to_string(),
    };
    let exists = match record.exists {
        Some(class) => format!("{class:?}"),
        None => "Missing".to_string(),
    };
    format!(
        "poll#{} t+{}-{}ms inspect then t+{}-{}ms exists order=inspect-then-exists inspect={inspect} exists={exists} late={} {}",
        record.index,
        record.inspect_start_ms,
        record.inspect_end_ms,
        record.exists_start_ms,
        record.exists_end_ms,
        record.late_rejected,
        record.note,
    )
}

/// Bounded poll history with per-outcome inspect and exists
/// counts, outcome-transition tracking, first-anomaly offset,
/// and first/last capped records. The full trace is never
/// retained.
#[derive(Debug, Default)]
pub struct PollHistory {
    pub total: usize,
    pub first_anomaly_ms: Option<u64>,
    pub inspect_clean: usize,
    pub inspect_changed: usize,
    pub inspect_unqueryable: usize,
    pub inspect_unparseable: usize,
    pub inspect_missing: usize,
    pub exists_found: usize,
    pub exists_notfound: usize,
    pub exists_qf: usize,
    pub exists_missing: usize,
    pub late_rejected: usize,
    pub query_failure_polls: usize,
    pub transitions: usize,
    last_key: Option<(InspectClass, ExistsClass)>,
    first: VecDeque<String>,
    last: VecDeque<String>,
}

fn anomaly_of(inspect: InspectClass, exists: ExistsClass) -> bool {
    !matches!(inspect, InspectClass::CleanSame | InspectClass::Changed)
        || !matches!(exists, ExistsClass::Found)
}

/// Observe one rendered poll: counts, transitions on the
/// (inspect, exists) outcome tuple, first-anomaly offset.
/// Pure.
pub fn history_observe(
    history: &mut PollHistory,
    rendered: String,
    inspect: InspectClass,
    exists: ExistsClass,
    late: bool,
    t_ms: u64,
) {
    let key = (inspect, exists);
    if let Some(last) = history.last_key
        && last != key
    {
        history.transitions += 1;
    }
    history.last_key = Some(key);
    if late {
        history.late_rejected += 1;
    }
    let query_failure = matches!(
        inspect,
        InspectClass::Unqueryable | InspectClass::Unparseable
    ) || matches!(exists, ExistsClass::QueryFailure);
    if query_failure {
        history.query_failure_polls += 1;
    }
    let anomaly = anomaly_of(inspect, exists) || late;
    if anomaly && history.first_anomaly_ms.is_none() {
        history.first_anomaly_ms = Some(t_ms);
    }
    match inspect {
        InspectClass::CleanSame => history.inspect_clean += 1,
        InspectClass::Changed => history.inspect_changed += 1,
        InspectClass::Unqueryable => history.inspect_unqueryable += 1,
        InspectClass::Unparseable => history.inspect_unparseable += 1,
        InspectClass::Missing => history.inspect_missing += 1,
    }
    match exists {
        ExistsClass::Found => history.exists_found += 1,
        ExistsClass::NotFound => history.exists_notfound += 1,
        ExistsClass::QueryFailure => history.exists_qf += 1,
        ExistsClass::Missing => history.exists_missing += 1,
    }
    history.total += 1;
    if history.first.len() < HISTORY_KEEP_EACH_END {
        history.first.push_back(rendered.clone());
    }
    history.last.push_back(rendered);
    while history.last.len() > HISTORY_KEEP_EACH_END {
        history.last.pop_front();
    }
}

/// Render the history report. Pure.
#[must_use]
pub fn report_history(history: &PollHistory) -> String {
    let mut out = format!(
        "polls={} inspect(clean={} changed={} unqueryable={} unparseable={} missing={}) exists(found={} notfound={} queryfail={} missing={}) transitions={} late_rejected={} qf_polls={}",
        history.total,
        history.inspect_clean,
        history.inspect_changed,
        history.inspect_unqueryable,
        history.inspect_unparseable,
        history.inspect_missing,
        history.exists_found,
        history.exists_notfound,
        history.exists_qf,
        history.exists_missing,
        history.transitions,
        history.late_rejected,
        history.query_failure_polls,
    );
    match history.first_anomaly_ms {
        Some(ms) => out.push_str(&format!(" first_anomaly=+{ms}ms")),
        None => out.push_str(" first_anomaly=none"),
    }
    out.push_str(" first=[");
    out.push_str(
        &history
            .first
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push_str("] last=[");
    out.push_str(&history.last.iter().cloned().collect::<Vec<_>>().join(" | "));
    out.push(']');
    out
}

/// Conditional unqueryable section: query failures are stated
/// as counts over polls (some-vs-all), never as proven
/// absence; a timeout on unchanged identity is landing-not-
/// established; a timeout past a rejected late change says so.
/// Pure.
#[must_use]
pub fn unqueryable_section(history: &PollHistory) -> String {
    if history.query_failure_polls > 0 {
        return format!(
            "container not successfully queried in {}/{total} polls (first at +{ms}ms); {clean} polls observed unchanged identity; absence not established",
            history.query_failure_polls,
            total = history.total,
            ms = history.first_anomaly_ms.unwrap_or(0),
            clean = history.inspect_clean,
        );
    }
    let missing = history.inspect_missing + history.exists_missing;
    if history.late_rejected > 0 {
        return format!(
            "a changed identity was observed past the deadline and rejected ({}); landing not established; absence not established",
            history.late_rejected
        );
    }
    if missing > 0 && history.total == missing {
        return "no probe could be scheduled; absence not established".to_string();
    }
    if history.inspect_clean > 0 {
        return format!(
            "landing not established (identity unchanged across {} polls); absence not established",
            history.inspect_clean
        );
    }
    if history.exists_notfound > 0 {
        return format!(
            "container observed absent (exists=1) in {}/{} polls",
            history.exists_notfound, history.total
        );
    }
    "no usable observation; absence not established".to_string()
}

/// Absolute probe window: the probe may start at `now` and run
/// at most `exec_allowance` of execution with `reserve` for
/// harness cleanup, all inside `phase_deadline`. Returns the
/// absolute deadline for the command runner, or `None` when
/// not even one full window fits (probe unscheduled —
/// reported MISSING, never fabricated). Pure.
#[must_use]
pub fn probe_window(
    now: Instant,
    phase_deadline: Instant,
    exec_allowance: Duration,
    reserve: Duration,
) -> Option<Instant> {
    let need = exec_allowance.checked_add(reserve)?;
    let end = now.checked_add(need)?;
    if end <= phase_deadline {
        Some(end)
    } else {
        None
    }
}

/// Timeout marker: timeouts are labeled as timeouts with the
/// actual allowance granted, never counted as backend
/// outcomes. Pure.
#[must_use]
pub fn timeout_mark(what: &str, allowed: Duration) -> String {
    format!("{what} timed out after {allowed:?}; not a backend outcome")
}

/// Render one collected command's evidence line from its end
/// state and raw streams. Pure.
#[must_use]
pub fn render_outcome(argv: &str, outcome: &ProbeOutcome, stream_cap: usize) -> String {
    render_cmd_evidence(
        argv,
        &format!(
            "{:?} stdout={} stderr={}",
            outcome.end,
            escape_capped(&outcome.stdout, stream_cap),
            escape_capped(&outcome.stderr, stream_cap),
        ),
    )
}

/// Render one collected command's evidence: either its
/// disposition plus capped streams, or an explicit
/// missing/timed-out marker. Pure.
#[must_use]
pub fn render_cmd_evidence(argv: &str, outcome: &str) -> String {
    format!("cmd[{argv}] :: {outcome}")
}

/// Render the timeout verdict block: the original verdict
/// line first, the unqueryable section directly adjacent.
/// Later-healthy observations can only append after this
/// block, never replace it. Pure.
#[must_use]
pub fn render_timeout_report(
    verdict: &str,
    section: &str,
    baseline: &str,
    history: &str,
    postmortem: &str,
) -> String {
    format!(
        "{verdict}\n{section}\nbaseline: {baseline}\nhistory: {history}\npostmortem: {postmortem}"
    )
}

/// The running systemd unit for a container name. Quadlet
/// source files are `<name>.container`; the running unit is
/// `<name>.service` — querying the source filename reports a
/// nonexistent unit instead of the service state.
#[must_use]
pub fn unit_name(container: &str) -> String {
    format!("{container}.service")
}

/// Command builders: the exact argv the collector runs, so
/// pins assert the real invocation shape (program, flags,
/// explicit properties, known identities).
#[must_use]
pub fn inspect_argv(container: &str) -> Vec<String> {
    vec![
        "podman".to_string(),
        "inspect".to_string(),
        "--format".to_string(),
        "{{.Id}} {{.State.StartedAt}}".to_string(),
        container.to_string(),
    ]
}

/// `container exists` has no output contract beyond its code.
#[must_use]
pub fn exists_argv(container: &str) -> Vec<String> {
    vec![
        "podman".to_string(),
        "container".to_string(),
        "exists".to_string(),
        container.to_string(),
    ]
}

/// Unit properties are selected explicitly so they survive
/// caps: show-everything-then-truncate could drop the fields
/// the post-mortem must compare.
#[must_use]
pub fn show_argv(unit: &str) -> Vec<String> {
    vec![
        "systemctl".to_string(),
        "--user".to_string(),
        "show".to_string(),
        unit.to_string(),
        "--property".to_string(),
        "LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,MainPID,ControlPID,InvocationID,NRestarts,Restart,SuccessExitStatus,ActiveEnterTimestampMonotonic,InactiveEnterTimestampMonotonic,StartLimitBurst,StartLimitIntervalUSec"
            .to_string(),
    ]
}

/// `ps` narrows by container name; the known container ID is
/// checked against the output in Rust (both identities
/// preserved, neither assumed from the other).
#[must_use]
pub fn ps_argv(container: &str) -> Vec<String> {
    vec![
        "podman".to_string(),
        "ps".to_string(),
        "-a".to_string(),
        "--filter".to_string(),
        format!("name={container}"),
        "--format".to_string(),
        "{{.ID}} {{.Names}} {{.State}}".to_string(),
    ]
}

/// Journal slice bounded from the recorded pre-surgery cursor.
#[must_use]
pub fn journal_argv(unit: &str, since_epoch_secs: u64) -> Vec<String> {
    vec![
        "journalctl".to_string(),
        "--user".to_string(),
        "-u".to_string(),
        unit.to_string(),
        "--since".to_string(),
        format!("@{since_epoch_secs}"),
        "--no-pager".to_string(),
        "-n".to_string(),
        "100".to_string(),
    ]
}

#[cfg(test)]
mod surgery_diag_tests {
    // Backend-free pins for the collector taxonomy: every
    // test below invokes the same functions the live path
    // calls, with synthetic inputs (no podman, no systemd).
    use super::*;

    fn ok_out(code: i32, stdout: &str, stderr: &str) -> ProbeOutcome {
        use std::os::unix::process::ExitStatusExt;
        let raw = if code == 0 { 0 } else { code << 8 };
        split_outcome(Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(raw),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }))
    }

    #[test]
    fn exists_codes_follow_the_cli_contract() {
        assert_eq!(
            classify_exists(Some(&ok_out(0, "", ""))),
            ExistsClass::Found
        );
        assert_eq!(
            classify_exists(Some(&ok_out(1, "", ""))),
            ExistsClass::NotFound
        );
        // 125 and every other end state are query failures,
        // never absence. Unscheduled is Missing, never a
        // fabricated query failure.
        assert_eq!(
            classify_exists(Some(&ok_out(125, "", ""))),
            ExistsClass::QueryFailure
        );
        assert_eq!(classify_exists(None), ExistsClass::Missing);
        assert_eq!(
            classify_exists(Some(&ProbeOutcome {
                end: ProbeEnd::TimedOut,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })),
            ExistsClass::QueryFailure
        );
        assert_eq!(
            classify_exists(Some(&ProbeOutcome {
                end: ProbeEnd::SpawnFailed,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })),
            ExistsClass::QueryFailure
        );
    }

    #[test]
    fn inspect_compares_whole_pairs_with_started_at_intact() {
        // Spaceful StartedAt (podman 5.7) must survive as one
        // field: bound and observation share it exactly.
        let bound = ("abc123", "2026-10-25 23:51:07.581067683 +0000 UTC");
        let (class, pair) = classify_inspect(
            Some(&ok_out(
                0,
                "abc123 2026-10-25 23:51:07.581067683 +0000 UTC\n",
                "",
            )),
            bound.0,
            bound.1,
        );
        assert_eq!(class, InspectClass::CleanSame);
        assert_eq!(pair, Some(("abc123".to_string(), bound.1.to_string())));
        // Changed StartedAt with the same Id lands.
        let (class, _) = classify_inspect(
            Some(&ok_out(
                0,
                "abc123 2026-10-25 23:52:00.000000000 +0000 UTC\n",
                "",
            )),
            bound.0,
            bound.1,
        );
        assert_eq!(class, InspectClass::Changed);
        // Changed Id with the same StartedAt also lands.
        let (class, _) = classify_inspect(
            Some(&ok_out(
                0,
                "def456 2026-10-25 23:51:07.581067683 +0000 UTC\n",
                "",
            )),
            bound.0,
            bound.1,
        );
        assert_eq!(class, InspectClass::Changed);
    }

    #[test]
    fn inspect_nonzero_is_unqueryable_not_absence() {
        let (class, pair) = classify_inspect(Some(&ok_out(125, "", "")), "id", "st");
        assert_eq!(class, InspectClass::Unqueryable);
        assert_eq!(pair, None);
        let (class, pair) = classify_inspect(
            Some(&ProbeOutcome {
                end: ProbeEnd::TimedOut,
                stdout: b"partial".to_vec(),
                stderr: Vec::new(),
            }),
            "id",
            "st",
        );
        assert_eq!(class, InspectClass::Unqueryable);
        assert_eq!(pair, None);
    }

    #[test]
    fn inspect_unparseable_is_its_own_class() {
        let (class, pair) = classify_inspect(Some(&ok_out(0, "\n", "")), "id", "st");
        assert_eq!(class, InspectClass::Unparseable);
        assert_eq!(pair, None);
    }

    #[test]
    fn unscheduled_probes_are_missing_not_query_failures() {
        let (class, pair) = classify_inspect(None, "id", "st");
        assert_eq!(class, InspectClass::Missing);
        assert_eq!(pair, None);
    }

    #[test]
    fn escape_keeps_one_line_with_explicit_truncation() {
        assert_eq!(escape_capped(b"abc\ndef", 160), "abc\\ndef");
        // Whitespace is preserved, not trimmed away.
        assert_eq!(escape_capped(b"  spaced  \n", 160), "  spaced  \\n");
        // Truncation names the dropped count.
        assert_eq!(escape_capped(b"0123456789", 4), "0123...[+6 truncated]");
        // Unfamiliar strings survive verbatim (lossy), capped.
        assert_eq!(
            escape_capped(b"no such object: x", 160),
            "no such object: x"
        );
        assert_eq!(escape_capped(b"", 160), "");
        // Terminal controls are escaped, never passed through.
        assert_eq!(escape_capped(b"a\x1bb", 160), "a\\x1bb");
        assert_eq!(escape_capped(b"a\x07b", 160), "a\\x07b");
    }

    #[test]
    fn history_keeps_first_and_last_with_counts() {
        let mut history = PollHistory::default();
        for i in 0..10u64 {
            let rendered = format!("poll#{i}");
            let inspect = if i >= 3 {
                InspectClass::Unqueryable
            } else {
                InspectClass::CleanSame
            };
            history_observe(
                &mut history,
                rendered,
                inspect,
                ExistsClass::Found,
                false,
                i * 500,
            );
        }
        assert_eq!(history.total, 10);
        assert_eq!(history.first_anomaly_ms, Some(1500));
        // One clean-to-anomaly outcome transition at poll 3.
        assert_eq!(history.transitions, 1);
        assert_eq!(history.inspect_unqueryable, 7);
        assert_eq!(history.inspect_clean, 3);
        assert_eq!(history.exists_found, 10);
        let report = report_history(&history);
        assert!(report.contains("polls=10"), "{report}");
        assert!(report.contains("first_anomaly=+1500ms"), "{report}");
        assert!(
            report.contains("poll#0") && report.contains("poll#2"),
            "{report}"
        );
        assert!(report.contains("poll#9"), "{report}");
        assert!(!report.contains("poll#5"), "{report}");
    }

    #[test]
    fn history_tracks_exists_flaps_as_transitions() {
        let mut history = PollHistory::default();
        history_observe(
            &mut history,
            "a".to_string(),
            InspectClass::CleanSame,
            ExistsClass::Found,
            false,
            0,
        );
        history_observe(
            &mut history,
            "b".to_string(),
            InspectClass::CleanSame,
            ExistsClass::NotFound,
            false,
            500,
        );
        history_observe(
            &mut history,
            "c".to_string(),
            InspectClass::CleanSame,
            ExistsClass::Found,
            false,
            1000,
        );
        // Two tuple transitions (Found->NotFound->Found) even
        // though every inspect stayed clean.
        assert_eq!(history.transitions, 2);
        assert_eq!(history.exists_notfound, 1);
        assert_eq!(history.exists_found, 2);
    }

    #[test]
    fn unqueryable_section_is_conditional_and_honest() {
        // Query failures among clean polls: some-vs-all counts.
        let mut history = PollHistory::default();
        history_observe(
            &mut history,
            "q".to_string(),
            InspectClass::Unqueryable,
            ExistsClass::QueryFailure,
            false,
            2500,
        );
        history_observe(
            &mut history,
            "c".to_string(),
            InspectClass::CleanSame,
            ExistsClass::Found,
            false,
            3000,
        );
        let section = unqueryable_section(&history);
        assert!(
            section.contains("not successfully queried in 1/2"),
            "{section}"
        );
        assert!(section.contains("absence not established"), "{section}");
        // Timeout on unchanged identity only: landing not
        // established, never never-queried.
        let mut clean = PollHistory::default();
        history_observe(
            &mut clean,
            "c".to_string(),
            InspectClass::CleanSame,
            ExistsClass::Found,
            false,
            0,
        );
        let section = unqueryable_section(&clean);
        assert!(section.contains("landing not established"), "{section}");
        assert!(!section.contains("not successfully queried"), "{section}");
    }

    #[test]
    fn probe_window_composes_allowance_inside_phase() {
        let start = Instant::now();
        let reserve = Duration::from_secs(20);
        // Full window fits: absolute deadline carries the
        // stated execution allowance plus the reserve.
        assert_eq!(
            probe_window(
                start,
                start + Duration::from_secs(60),
                Duration::from_secs(5),
                reserve
            ),
            Some(start + Duration::from_secs(25))
        );
        // Not even one full window fits: unscheduled, never a
        // fabricated short window.
        assert_eq!(
            probe_window(
                start,
                start + Duration::from_secs(10),
                Duration::from_secs(5),
                reserve
            ),
            None
        );
        assert_eq!(
            probe_window(start, start, Duration::from_secs(5), reserve),
            None
        );
    }

    #[test]
    fn timeout_mark_names_allowance_not_backend() {
        let mark = timeout_mark("inspect probe", Duration::from_secs(5));
        assert!(mark.contains("timed out after 5s"), "{mark}");
        assert!(mark.contains("not a backend outcome"), "{mark}");
    }

    #[test]
    fn timeout_report_leads_with_original_verdict() {
        let report = render_timeout_report(
            "surgery never landed a new incarnation for X",
            "container not successfully queried",
            "base",
            "hist",
            "pm",
        );
        let verdict_at = report
            .find("surgery never landed")
            .expect("verdict present");
        let section_at = report
            .find("not successfully queried")
            .expect("section present");
        assert!(verdict_at < section_at, "{report}");
    }

    #[test]
    fn builders_name_the_real_unit_and_properties() {
        let unit = unit_name("cistella-abc");
        assert_eq!(unit, "cistella-abc.service");
        let show = show_argv(&unit);
        assert_eq!(show[0], "systemctl");
        assert!(show.contains(&unit), "{show:?}");
        for prop in [
            "LoadState",
            "ActiveState",
            "SubState",
            "Result",
            "MainPID",
            "InvocationID",
            "NRestarts",
            "StartLimitBurst",
        ] {
            assert!(show.join(" ").contains(prop), "missing {prop}: {show:?}");
        }
        let ps = ps_argv("cistella-abc");
        assert!(ps.join(" ").contains("name=cistella-abc"), "{ps:?}");
        let journal = journal_argv(&unit, 123);
        assert!(journal.contains(&"@123".to_string()), "{journal:?}");
        assert!(journal.contains(&unit), "{journal:?}");
    }
}
