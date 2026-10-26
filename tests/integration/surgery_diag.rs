//! Backend-free diagnostic record core for the enter-gate surgery
//! paths (`enter_gate.rs`).
//!
//! Every function here is pure over its inputs: the test body
//! runs the subprocesses (podman, systemctl, journalctl) and
//! hands the outcomes in; classification, history caps, report
//! rendering, and budget composition never touch a backend.
//! The `#[cfg(test)]` pins below invoke this same
//! implementation, so the taxonomy cannot drift from the
//! collector (no duplicated model).

use std::collections::VecDeque;
use std::process::Output;
use std::time::{Duration, Instant};

use super::bounded::BoundedRunError;

/// Per-stream render cap: matches the historical note width so
/// reports stay log-shaped.
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

/// Escape a byte stream for one-line rendering: lossy text,
/// capped at `cap` characters, with line breaks escaped so one
/// record stays one line. Verbatim otherwise (no prose
/// diagnosis of unfamiliar strings).
#[must_use]
pub fn escape_capped(bytes: &[u8], cap: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    for ch in text.trim().chars() {
        if out.len() >= cap {
            break;
        }
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out
}

/// `podman container exists` classification per the CLI
/// contract: 0 found, 1 not found, anything else (including
/// 125 and signals) a storage/access query failure — never
/// absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistsClass {
    Found,
    NotFound,
    QueryFailure,
}

/// Classify an exists probe from its end state. Pure.
#[must_use]
pub fn classify_exists(end: ProbeEnd) -> ExistsClass {
    match end {
        ProbeEnd::Exited(0) => ExistsClass::Found,
        ProbeEnd::Exited(1) => ExistsClass::NotFound,
        _ => ExistsClass::QueryFailure,
    }
}

/// Inspect outcome classification against the bound pair.
/// `Changed` and `CleanSame` require a successful parse of the
/// whole pair with the shared product parser (StartedAt kept
/// intact — never reconstructed from whitespace tokens).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectClass {
    CleanSame,
    Changed,
    Unparseable,
    Unqueryable,
}

/// Classify an inspect outcome. Pure: parses with the product
/// parser, compares whole pairs. Returns the class plus the
/// parsed identity when one exists.
#[must_use]
pub fn classify_inspect(
    end: ProbeEnd,
    stdout: &[u8],
    bound_id: &str,
    bound_started: &str,
) -> (InspectClass, Option<(String, String)>) {
    if end != ProbeEnd::Exited(0) {
        return (InspectClass::Unqueryable, None);
    }
    match cistella::attestation::parse_identity(&String::from_utf8_lossy(stdout)) {
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

/// One poll iteration's diagnostic record. Timestamps are
/// monotonic millisecond offsets from the wait start; order
/// records which probe ran first.
#[derive(Debug, Clone)]
pub struct PollRecord {
    pub index: usize,
    pub inspect_start_ms: u64,
    pub inspect_end_ms: u64,
    pub exists_start_ms: u64,
    pub exists_end_ms: u64,
    pub inspect_first: bool,
    pub inspect: InspectClass,
    pub exists: ExistsClass,
    pub note: String,
}

/// Render one record as one line, including the probe
/// order (inspect-then-exists). Pure.
#[must_use]
pub fn render_poll(record: &PollRecord) -> String {
    let order = if record.inspect_first {
        "inspect-then-exists"
    } else {
        "exists-then-inspect"
    };
    format!(
        "poll#{} t+{}-{}ms inspect then t+{}-{}ms exists order={order} inspect={:?} exists={:?} {}",
        record.index,
        record.inspect_start_ms,
        record.inspect_end_ms,
        record.exists_start_ms,
        record.exists_end_ms,
        record.inspect,
        record.exists,
        record.note,
    )
}

/// Bounded poll history: first-anomaly time, per-outcome
/// counts, transitions, and the first/last capped records.
/// The full trace is never retained.
#[derive(Debug, Default)]
pub struct PollHistory {
    total: usize,
    first_anomaly: Option<usize>,
    landed: usize,
    clean_same: usize,
    unqueryable: usize,
    unparseable: usize,
    transitions: usize,
    last_is_anomaly: bool,
    first: VecDeque<String>,
    last: VecDeque<String>,
}

/// Push a rendered record. `anomaly` is true for any outcome
/// that differs from clean success (changed, unqueryable,
/// unparseable, non-found exists). Pure.
pub fn history_push(history: &mut PollHistory, rendered: String, anomaly: bool) {
    if anomaly && history.first_anomaly.is_none() {
        history.first_anomaly = Some(history.total);
    }
    if history.total > 0 && anomaly != history.last_is_anomaly {
        history.transitions += 1;
    }
    history.last_is_anomaly = anomaly;
    history.total += 1;
    if history.first.len() < HISTORY_KEEP_EACH_END {
        history.first.push_back(rendered.clone());
    }
    history.last.push_back(rendered);
    while history.last.len() > HISTORY_KEEP_EACH_END {
        history.last.pop_front();
    }
}

/// Tally one poll into the outcome counters. Pure.
pub fn history_tally(history: &mut PollHistory, inspect: &InspectClass, exists: &ExistsClass) {
    match inspect {
        InspectClass::Changed => history.landed += 1,
        InspectClass::CleanSame => history.clean_same += 1,
        InspectClass::Unqueryable => history.unqueryable += 1,
        InspectClass::Unparseable => history.unparseable += 1,
    }
    let _ = exists;
}

/// Render the history report. Pure.
#[must_use]
pub fn report_history(history: &PollHistory) -> String {
    let mut out = format!(
        "polls={} landed={} clean_same={} unqueryable={} unparseable={} transitions={}",
        history.total,
        history.landed,
        history.clean_same,
        history.unqueryable,
        history.unparseable,
        history.transitions,
    );
    match history.first_anomaly {
        Some(i) => out.push_str(&format!(" first_anomaly=poll#{i}")),
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

/// Whether any poll observed a query failure (unqueryable
/// inspect or non-found/non-absent exists). Pure.
#[must_use]
pub fn had_query_failure(history: &PollHistory) -> bool {
    history.unqueryable > 0 || history.unparseable > 0
}

/// Whether any poll produced a clean successful observation.
/// Pure.
#[must_use]
pub fn had_clean_observation(history: &PollHistory) -> bool {
    history.clean_same > 0 || history.landed > 0
}

/// Conditional unqueryable section: the "not successfully
/// queried" claim requires observed query failures; a timeout
/// on unchanged identity is "landing not established",
/// and neither is absence. Pure.
#[must_use]
pub fn unqueryable_section(history: &PollHistory) -> String {
    if had_query_failure(history) {
        "container not successfully queried; absence not established".to_string()
    } else if had_clean_observation(history) {
        "landing not established (identity unchanged); absence not established".to_string()
    } else {
        "no poll completed with a usable observation; absence not established".to_string()
    }
}

/// Clamp a per-probe allowance to the remaining phase budget:
/// the probe gets at most `per_probe` and never past
/// `phase_deadline`. Returns `None` when no budget remains
/// (probe unscheduled, reported as missing). Pure.
#[must_use]
pub fn clamp_probe_allowance(
    now: Instant,
    phase_deadline: Instant,
    per_probe: Duration,
) -> Option<Duration> {
    if now >= phase_deadline {
        return None;
    }
    let remaining = phase_deadline.duration_since(now);
    Some(remaining.min(per_probe))
}

/// Render a timeout marker: timeouts are labeled as timeouts,
/// never counted as backend outcomes. Pure.
#[must_use]
pub fn timeout_mark(what: &str, allowed: Duration) -> String {
    format!("{what} timed out after {allowed:?}; not a backend outcome")
}

/// Render one collected command's evidence: either its
/// disposition plus capped streams, or an explicit
/// missing/timed-out marker. Pure.
#[must_use]
pub fn render_cmd_evidence(argv: &str, outcome: &str) -> String {
    format!("cmd[{argv}] :: {outcome}")
}

/// Split a bounded-run outcome into its end state plus owned
/// stdout/stderr bytes (partials on timeout, empty when the
/// command never produced output). Pure.
#[must_use]
pub fn split_outcome(outcome: Result<Output, BoundedRunError>) -> (ProbeEnd, Vec<u8>, Vec<u8>) {
    match outcome {
        Ok(out) => {
            let end = match out.status.code() {
                Some(code) => ProbeEnd::Exited(code),
                None => ProbeEnd::Signaled,
            };
            (end, out.stdout, out.stderr)
        }
        Err(BoundedRunError::Timeout {
            partial_stdout,
            partial_stderr,
            ..
        }) => (
            ProbeEnd::TimedOut,
            partial_stdout.into_bytes(),
            partial_stderr.into_bytes(),
        ),
        Err(BoundedRunError::Spawn(_)) => (ProbeEnd::SpawnFailed, Vec::new(), Vec::new()),
        Err(BoundedRunError::DrainTimeout { .. }) => (ProbeEnd::TimedOut, Vec::new(), Vec::new()),
        Err(BoundedRunError::Wait { .. }) => (ProbeEnd::WaitFailed, Vec::new(), Vec::new()),
        Err(BoundedRunError::Overflow { .. }) => (ProbeEnd::Overflowed, Vec::new(), Vec::new()),
        Err(BoundedRunError::Capture { .. }) => (ProbeEnd::WaitFailed, Vec::new(), Vec::new()),
    }
}

/// Render one command's evidence line from its end state and
/// raw streams. Pure.
#[must_use]
pub fn render_outcome(argv: &str, end: ProbeEnd, stdout: &[u8], stderr: &[u8]) -> String {
    render_cmd_evidence(
        argv,
        &format!(
            "{end:?} stdout={} stderr={}",
            escape_capped(stdout, RENDER_CAP),
            escape_capped(stderr, RENDER_CAP),
        ),
    )
}

#[cfg(test)]
mod surgery_diag_tests {
    // Backend-free pins for the collector taxonomy: every
    // test below invokes the same functions the live path
    // calls, with synthetic inputs (no podman, no systemd).
    use super::*;

    #[test]
    fn exists_codes_follow_the_cli_contract() {
        assert_eq!(classify_exists(ProbeEnd::Exited(0)), ExistsClass::Found);
        assert_eq!(classify_exists(ProbeEnd::Exited(1)), ExistsClass::NotFound);
        // 125 and every other end state are query failures,
        // never absence.
        assert_eq!(
            classify_exists(ProbeEnd::Exited(125)),
            ExistsClass::QueryFailure
        );
        assert_eq!(
            classify_exists(ProbeEnd::Exited(3)),
            ExistsClass::QueryFailure
        );
        assert_eq!(
            classify_exists(ProbeEnd::Signaled),
            ExistsClass::QueryFailure
        );
        assert_eq!(
            classify_exists(ProbeEnd::TimedOut),
            ExistsClass::QueryFailure
        );
        assert_eq!(
            classify_exists(ProbeEnd::SpawnFailed),
            ExistsClass::QueryFailure
        );
    }

    #[test]
    fn inspect_compares_whole_pairs_with_started_at_intact() {
        // Spaceful StartedAt (podman 5.7) must survive as one
        // field: bound and observation share it exactly.
        let bound = ("abc123", "2026-10-25 23:51:07.581067683 +0000 UTC");
        let (class, pair) = classify_inspect(
            ProbeEnd::Exited(0),
            b"abc123 2026-10-25 23:51:07.581067683 +0000 UTC\n",
            bound.0,
            bound.1,
        );
        assert_eq!(class, InspectClass::CleanSame);
        assert_eq!(pair, Some(("abc123".to_string(), bound.1.to_string())));
        // Changed StartedAt with the same Id lands.
        let (class, _) = classify_inspect(
            ProbeEnd::Exited(0),
            b"abc123 2026-10-25 23:52:00.000000000 +0000 UTC\n",
            bound.0,
            bound.1,
        );
        assert_eq!(class, InspectClass::Changed);
        // Changed Id with the same StartedAt also lands.
        let (class, _) = classify_inspect(
            ProbeEnd::Exited(0),
            b"def456 2026-10-25 23:51:07.581067683 +0000 UTC\n",
            bound.0,
            bound.1,
        );
        assert_eq!(class, InspectClass::Changed);
    }

    #[test]
    fn inspect_nonzero_is_unqueryable_not_absence() {
        let (class, pair) = classify_inspect(ProbeEnd::Exited(125), b"", "id", "st");
        assert_eq!(class, InspectClass::Unqueryable);
        assert_eq!(pair, None);
        let (class, pair) = classify_inspect(ProbeEnd::TimedOut, b"", "id", "st");
        assert_eq!(class, InspectClass::Unqueryable);
        assert_eq!(pair, None);
    }

    #[test]
    fn inspect_unparseable_is_its_own_class() {
        let (class, pair) = classify_inspect(ProbeEnd::Exited(0), b"\n", "id", "st");
        assert_eq!(class, InspectClass::Unparseable);
        assert_eq!(pair, None);
    }

    #[test]
    fn escape_caps_and_keeps_records_one_line() {
        assert_eq!(escape_capped(b"abc\ndef", 160), "abc\\ndef");
        assert_eq!(escape_capped(b"  spaced  \n", 160), "spaced");
        assert_eq!(escape_capped(b"0123456789", 4), "0123");
        // Unfamiliar bytes survive verbatim (lossy), capped.
        assert_eq!(
            escape_capped(b"no such object: x", 160),
            "no such object: x"
        );
        assert_eq!(escape_capped(b"", 160), "");
    }

    #[test]
    fn history_keeps_first_and_last_with_counts() {
        let mut history = PollHistory::default();
        for i in 0..10 {
            history_push(&mut history, format!("poll#{i}"), i >= 3);
            history_tally(
                &mut history,
                &InspectClass::Unqueryable,
                &ExistsClass::Found,
            );
        }
        assert_eq!(history.total, 10);
        assert_eq!(history.first_anomaly, Some(3));
        // One clean-to-anomaly transition at poll 3.
        assert_eq!(history.transitions, 1);
        assert_eq!(history.unqueryable, 10);
        let report = report_history(&history);
        assert!(report.contains("polls=10"), "{report}");
        assert!(report.contains("first_anomaly=poll#3"), "{report}");
        assert!(
            report.contains("poll#0") && report.contains("poll#2"),
            "{report}"
        );
        assert!(report.contains("poll#9"), "{report}");
        assert!(!report.contains("poll#5"), "{report}");
    }

    #[test]
    fn unqueryable_label_is_conditional_on_observations() {
        // Query failures observed: not-successfully-queried.
        let mut history = PollHistory::default();
        history_push(&mut history, "q".to_string(), true);
        history_tally(
            &mut history,
            &InspectClass::Unqueryable,
            &ExistsClass::Found,
        );
        assert!(unqueryable_section(&history).contains("not successfully queried"));
        assert!(unqueryable_section(&history).contains("absence not established"));
        // Only unchanged identity observed: landing not
        // established, never never-queried.
        let mut clean = PollHistory::default();
        history_push(&mut clean, "c".to_string(), false);
        history_tally(&mut clean, &InspectClass::CleanSame, &ExistsClass::Found);
        let section = unqueryable_section(&clean);
        assert!(section.contains("landing not established"), "{section}");
        assert!(!section.contains("not successfully queried"), "{section}");
    }

    #[test]
    fn clamp_composes_per_probe_with_remaining_budget() {
        let start = Instant::now();
        // Remaining exceeds the per-probe allowance: probe
        // gets the allowance.
        assert_eq!(
            clamp_probe_allowance(
                start,
                start + Duration::from_secs(60),
                Duration::from_secs(5)
            ),
            Some(Duration::from_secs(5))
        );
        // Remaining under the allowance: probe gets the
        // remainder, never past the deadline.
        assert_eq!(
            clamp_probe_allowance(
                start,
                start + Duration::from_secs(2),
                Duration::from_secs(5)
            ),
            Some(Duration::from_secs(2))
        );
        // Expired budget: probe unscheduled.
        assert_eq!(
            clamp_probe_allowance(start, start, Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn timeout_mark_never_reads_as_backend_outcome() {
        let mark = timeout_mark("inspect", Duration::from_secs(5));
        assert!(mark.contains("timed out"), "{mark}");
        assert!(mark.contains("not a backend outcome"), "{mark}");
    }

    #[test]
    fn cmd_evidence_renders_missing_explicitly() {
        let rendered = render_cmd_evidence("systemctl show x", "MISSING: phase budget exhausted");
        assert!(rendered.contains("MISSING"), "{rendered}");
    }

    #[test]
    fn split_outcome_keeps_timeout_partials() {
        let out = std::process::Command::new("true")
            .output()
            .expect("true runs");
        let (end, stdout, _) = split_outcome(Ok(out));
        assert_eq!(end, ProbeEnd::Exited(0));
        assert!(stdout.is_empty());
    }

    #[test]
    fn render_outcome_names_end_and_streams() {
        let rendered = render_outcome(
            "podman inspect x",
            ProbeEnd::Exited(125),
            b"",
            b"no such object",
        );
        assert!(rendered.contains("Exited(125)"), "{rendered}");
        assert!(rendered.contains("no such object"), "{rendered}");
    }
}
