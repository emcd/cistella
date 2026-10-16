//! Harness-bound pins: bounded waits and reads, no systemd.
//!
//! Pins the production helpers (`read_conduct_id`, `run_bounded`,
//! `wait_bounded`) against their failure shapes — silent child,
//! flood, stuck poll, timeout cleanup — with deterministic stand-ins
//! (sockets, sleep) so the bounds themselves are proven, not merely
//! reviewed. Every pin asserts the bound: the call RETURNS within
//! its budget with a loud error, never hangs, never succeeds
//! silently, and leaves no zombie.

use std::os::unix::net::UnixStream;

use super::helpers::*;

/// Silent child (no bytes, no EOF — the pre-fix `read_line` would
/// block here forever): the bounded read returns `Timeout` within
/// its budget, never hangs past it.
#[test]
fn id_line_silent_child_times_out_within_budget() {
    let (reader, writer) = UnixStream::pair().expect("socket pair");
    // Writer deliberately held open: no bytes, no EOF.
    let start = std::time::Instant::now();
    let outcome = read_conduct_id(reader, std::time::Duration::from_millis(300));
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "silent-child read returned within budget, took {elapsed:?}"
    );
    assert!(
        matches!(outcome, Err(ConductIdError::Timeout { .. })),
        "silent child reads as timeout, not success: {outcome:?}"
    );
    drop(writer);
}

/// Dead child (EOF before any id line) reads as `EofWithoutId`,
/// promptly — distinct from the silent-child timeout so triage
/// knows which shape failed.
#[test]
fn id_line_eof_without_id_is_not_a_timeout() {
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    std::io::Write::write_all(&mut writer, b"startup noise\n").expect("write noise");
    drop(writer);
    let outcome = read_conduct_id(reader, std::time::Duration::from_secs(10));
    assert!(
        matches!(outcome, Err(ConductIdError::EofWithoutId)),
        "EOF without id line reads as eof, not timeout: {outcome:?}"
    );
}

/// Happy path preserved: noise lines skipped, `conduct <id>` line
/// accepted, bytes past the newline handed back for the drain.
#[test]
fn id_line_happy_path_keeps_noise_skip_and_leftover() {
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    let id = "a".repeat(cistella::session::MINTED_ID_LEN);
    std::io::Write::write_all(
        &mut writer,
        format!("startup noise\nconduct {id}\nTRAILING").as_bytes(),
    )
    .expect("write id line");
    let (found, _reader, leftover) =
        read_conduct_id(reader, std::time::Duration::from_secs(10)).expect("id line reads");
    assert_eq!(found, id, "minted id accepted");
    assert_eq!(
        leftover, b"TRAILING",
        "post-newline bytes retained for drain"
    );
    drop(writer);
}

/// Malformed `conduct ` line is a loud error, not a skip: shape
/// violations must never read as "id not yet arrived".
#[test]
fn id_line_malformed_id_is_loud() {
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    std::io::Write::write_all(&mut writer, b"conduct NOT-VALID!!\n").expect("write bad id");
    drop(writer);
    let outcome = read_conduct_id(reader, std::time::Duration::from_secs(10));
    assert!(
        matches!(outcome, Err(ConductIdError::MalformedId(_))),
        "malformed id line is loud: {outcome:?}"
    );
}

/// Stuck poll command (`sleep`, never exits): the bounded wait
/// RETURNS with a timeout inside its budget — and the child is
/// reaped (no zombie), attested by the error itself.
#[test]
fn stuck_poll_command_times_out_and_reaps() {
    let child = std::process::Command::new("sleep")
        .arg("60")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sleep");
    let start = std::time::Instant::now();
    let outcome = wait_bounded(child, std::time::Duration::from_millis(300));
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "stuck poll returned within budget, took {elapsed:?}"
    );
    match outcome {
        Err(BoundedRunError::Timeout { reaped, .. }) => assert!(
            reaped,
            "timeout-killed child is reaped: no zombie left behind"
        ),
        other => panic!("stuck poll reads as timeout: {other:?}"),
    }
}

/// Flood (200KB, past the 64KB pipe buffer — the shape that
/// deadlocks a wait-then-read design): full capture, byte-exact,
/// no truncation on the evidence path.
#[test]
fn flood_child_captured_fully_without_truncation() {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "exec head -c 200000 /dev/zero | exec tr '\\0' 'x'"]);
    let out = run_bounded(cmd, std::time::Duration::from_secs(20)).expect("flood run succeeds");
    assert!(out.status.success(), "flood child exited cleanly");
    assert_eq!(out.stdout.len(), 200000, "every flooded byte captured");
    assert!(
        out.stdout.iter().all(|b| *b == b'x'),
        "flood content byte-exact"
    );
}

/// Past-cap flood (6MB > 4MiB cap): killed LOUD with overflow, not
/// silently truncated — and promptly (cap trip, not full timeout).
#[test]
fn over_cap_flood_killed_loud_with_overflow() {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "exec head -c 6000000 /dev/zero"]);
    let start = std::time::Instant::now();
    let outcome = run_bounded(cmd, std::time::Duration::from_secs(30));
    let elapsed = start.elapsed();
    assert!(
        matches!(outcome, Err(BoundedRunError::Overflow { .. })),
        "over-cap flood reads as overflow, never truncated success: {outcome:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "overflow trips on the cap, not the timeout: {elapsed:?}"
    );
}

/// Caller-chosen null stderr (no pipe — the protocol_peer guest
/// shape): wait_bounded captures what IS piped and succeeds. The
/// absent stream is the test's own stdio choice, not truncated
/// evidence.
#[test]
fn unpiped_stream_needs_no_capture() {
    let child = std::process::Command::new("sh")
        .args(["-c", "echo hi"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn echo");
    let out = wait_bounded(child, std::time::Duration::from_secs(10)).expect("echo reaped");
    assert!(out.status.success(), "echo exited cleanly");
    assert_eq!(out.stdout, b"hi\n", "piped stream captured");
    assert!(
        out.stderr.is_empty(),
        "unpiped stream reads empty, not failed"
    );
}
