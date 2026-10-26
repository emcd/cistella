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
            detail: "waited=5s reaped=true".to_string(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })),
        ExistsClass::QueryFailure
    );
    assert_eq!(
        classify_exists(Some(&ProbeOutcome {
            end: ProbeEnd::SpawnFailed,
            detail: "spawn failed".to_string(),
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
            detail: "waited=5s reaped=true".to_string(),
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
        Some((start + Duration::from_secs(25), Duration::from_secs(5)))
    );
    // Small remaining budget: execution is clamped to what
    // remains (observations continue), cleanup accounted
    // past the phase bound as its own phase.
    assert_eq!(
        probe_window(
            start,
            start + Duration::from_secs(3),
            Duration::from_secs(5),
            reserve
        ),
        Some((start + Duration::from_secs(23), Duration::from_secs(3)))
    );
    // Nothing remains: unscheduled, never fabricated.
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
    assert!(ps.contains(&"--no-trunc".to_string()), "{ps:?}");
    let full = "abc123def456abc123def456abc123def456abc123def456abc123def456abcd";
    let listing = format!("{full} cistella-abc Up 2 minutes\n");
    assert!(ps_id_present(listing.as_bytes(), full));
    assert!(!ps_id_present(listing.as_bytes(), "abc123"));
    // A truncated 12-char ID prefix must NOT count as a match.
    assert!(!ps_id_present(
        b"abc123def456 cistella-abc Up 2 minutes\n",
        full
    ));
    let journal = journal_argv(&unit, 123);
    assert!(journal.contains(&"@123".to_string()), "{journal:?}");
    assert!(journal.contains(&unit), "{journal:?}");
}
