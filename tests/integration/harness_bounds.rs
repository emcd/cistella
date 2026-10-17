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

/// Lifetime-drain handoff pin (Tier-2 material finding on 29ae74a):
/// `read_conduct_id` leaves stdout O_NONBLOCK; the production
/// handoff must restore blocking BEFORE its one-shot copy, or the
/// first write gap ends the copy, drops the reader, and the child
/// dies SIGPIPE. This drives the ACTUAL helpers
/// (`restore_blocking` + `spawn_stdout_drain`, the exact
/// `spawn_conduct_tapped` sequence) through ID → 2s quiet gap →
/// later write → EOF, and attests every byte arrives. Pre-fix code
/// (one-shot copy on the nonblocking fd) delivers only the
/// leftover head and misses the post-gap write.
#[test]
fn stdout_drain_survives_quiet_gap_after_id_line() {
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    // Simulate the reader's postcondition exactly: O_NONBLOCK set.
    let flags =
        nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).expect("F_GETFL");
    let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
    oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags))
        .expect("O_NONBLOCK");
    // Test-owned sink (production passes io::sink()): the drained
    // bytes are observable here.
    let (sink_reader, sink_writer) = UnixStream::pair().expect("sink pair");
    // The production handoff sequence, in order.
    restore_blocking(&reader).expect("blocking restored");
    let drain = spawn_stdout_drain(reader, b"LEFTOVER".to_vec(), sink_writer);
    // Quiet gap LONGER than any copy poll: a nonblocking one-shot
    // copy would exit here. Then a later write, then EOF.
    std::thread::sleep(std::time::Duration::from_secs(2));
    writer.write_all(b"AFTER-GAP").expect("later write lands");
    drop(writer);
    drain.join().expect("drain thread reaped");
    let mut got = Vec::new();
    let mut sink_reader = sink_reader;
    sink_reader.read_to_end(&mut got).expect("sink drains");
    let got = String::from_utf8_lossy(&got).to_string();
    assert!(got.contains("LEFTOVER"), "leftover head delivered: {got:?}");
    assert!(
        got.contains("AFTER-GAP"),
        "post-gap write delivered (no WouldBlock copy exit): {got:?}"
    );
}

/// Continuously-writing newline-free producer (no id, no EOF):
/// the absolute deadline fires on EVERY iteration, so this returns
/// `Timeout` within budget instead of spinning past it on
/// successful reads. The writer thread parks after the test
/// (detached, same precedent as held-open pins).
#[test]
fn id_line_continuous_producer_still_times_out() {
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    std::thread::spawn(move || {
        for _ in 0..100 {
            let _ = std::io::Write::write_all(&mut writer, &[b'x'; 1024]);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    let start = std::time::Instant::now();
    let outcome = read_conduct_id(reader, std::time::Duration::from_millis(300));
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "continuous producer returned within budget, took {elapsed:?}"
    );
    assert!(
        matches!(outcome, Err(ConductIdError::Timeout { .. })),
        "continuous no-id output reads as timeout: {outcome:?}"
    );
}

/// Runaway producer (megabytes, no newline): the pending cap fires
/// LOUD with `LineOverflow` instead of growing the buffer without
/// bound. Fast writer fills 64 KiB in milliseconds.
#[test]
fn id_line_runaway_producer_hits_pending_cap() {
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    std::thread::spawn(move || {
        for _ in 0..1000 {
            if std::io::Write::write_all(&mut writer, &[b'y'; 4096]).is_err() {
                break;
            }
        }
    });
    let start = std::time::Instant::now();
    let outcome = read_conduct_id(reader, std::time::Duration::from_secs(30));
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "runaway producer capped promptly, took {elapsed:?}"
    );
    assert!(
        matches!(outcome, Err(ConductIdError::LineOverflow { .. })),
        "runaway output reads as overflow, not hang: {outcome:?}"
    );
}

/// Reader that yields two chunks then fails: injected read error
/// for the capture path (a real `Child` pipe cannot be made to
/// fail deterministically, so this drives `drain_capped` +
/// `finish_capture` — the exact production functions — directly).
struct FailAfter {
    chunks: usize,
}

impl std::io::Read for FailAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.chunks == 0 {
            return Err(std::io::Error::other("injected capture failure"));
        }
        self.chunks -= 1;
        let n = 8.min(buf.len());
        buf[..n].copy_from_slice(&b"12345678"[..n]);
        Ok(n)
    }
}

/// Failed capture refuses `Ok`: a read error before EOF surfaces
/// as `Err(Capture)` with the partial bytes as labeled triage —
/// never as successful evidence. Pre-fix code returned the
/// partial bytes as `Ok(Output)`.
#[test]
fn failed_capture_refuses_success() {
    use std::sync::{Arc, Mutex};
    let out_slot = Arc::new(Mutex::new(StreamCapture::empty()));
    let err_slot = Arc::new(Mutex::new(StreamCapture::empty()));
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = drain_capped(FailAfter { chunks: 2 }, Arc::clone(&out_slot), tx);
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("drain reports completion");
    handle.join().expect("drain thread reaped");
    assert!(
        out_slot
            .lock()
            .expect("capture mutex")
            .error
            .as_deref()
            .is_some_and(|e| e.contains("injected")),
        "read error recorded on the slot"
    );
    let status = std::os::unix::process::ExitStatusExt::from_raw(0);
    let outcome = finish_capture(status, &out_slot, &err_slot, true, true);
    match outcome {
        Err(BoundedRunError::Capture {
            stream,
            detail,
            partial_stdout,
            ..
        }) => {
            assert_eq!(stream, "stdout", "failing stream named");
            assert!(detail.contains("injected"), "cause retained: {detail}");
            assert!(
                partial_stdout.contains("12345678"),
                "partial bytes kept as triage: {partial_stdout:?}"
            );
        }
        other => panic!("failed capture must refuse Ok: {other:?}"),
    }
}

/// Multibyte codepoint straddling the 8 KiB diagnostic cut: 4097
/// é's (U+00E9, 2 bytes each) put a codepoint boundary at byte
/// 8192. The timeout path must render partials WITHOUT panicking
/// inside `str` indexing (pre-fix `&text[..8192]` panicked on the
/// split codepoint, masking the timeout itself).
#[test]
fn multibyte_at_truncation_boundary_does_not_panic() {
    let mut cmd = std::process::Command::new("sh");
    // 4097 two-byte é's = 8194 bytes, then hang for the timeout.
    cmd.args(["-c", "printf '\\303\\251%.0s' $(seq 1 4097); sleep 60"]);
    let start = std::time::Instant::now();
    let outcome = run_bounded(cmd, std::time::Duration::from_millis(500));
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "multibyte timeout returned within budget, took {elapsed:?}"
    );
    assert!(
        matches!(outcome, Err(BoundedRunError::Timeout { .. })),
        "multibyte partials read as timeout, never panic: {outcome:?}"
    );
}

/// Nonblocking-setup failure in `drain_pipe` (an O_PATH fd, whose
/// fcntl refuses): returns empty PROMPTLY instead of risking
/// blocking reads past the deadline. Diagnostics degrade
/// (logged), boundedness holds. The sub-second bound proves the
/// setup-failure branch: the read-error path would take the full
/// 2s deadline instead.
#[test]
fn drain_pipe_setup_failure_returns_empty_bounded() {
    use std::os::unix::fs::OpenOptionsExt;
    let probed = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_PATH.bits())
        .open("/")
        .expect("O_PATH open");
    let start = std::time::Instant::now();
    let out = drain_pipe(probed);
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "setup failure returns without reading, took {elapsed:?}"
    );
    assert!(out.is_empty(), "no diagnostics invented: {out:?}");
}

/// Held-writer pipe (exited parent, descendant holds the pipe —
/// the `sleep 30 &` shape): post-exit drain hits the bound and
/// returns labeled `DrainTimeout` instead of hanging in EOF.
/// Costs one 10s allowance; the orphaned sleeper exits alone.
#[test]
fn held_writer_pipe_drain_times_out_labeled() {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "sleep 30 &"]);
    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn shell");
    let start = std::time::Instant::now();
    let outcome = wait_bounded(child, std::time::Duration::from_secs(20));
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(25),
        "held-writer drain returned within allowances, took {elapsed:?}"
    );
    assert!(
        matches!(outcome, Err(BoundedRunError::DrainTimeout { .. })),
        "held pipe reads as drain timeout, never hang: {outcome:?}"
    );
}

/// Guard Drop during an assertion unwind preserves the original
/// panic: teardown is best-effort `Result`-typed, so a cleanup
/// failure can never double-panic and abort the process (masking
/// the original evidence and bypassing sibling cleanup). A
/// cleanup-timeout DURING unwind is not cheaply forgeable (no
/// fault seam for a 60s wedged terminate in a fast pin), so this
/// pins the structural invariant — unwind propagates with its
/// payload — while `guard_terminate`'s `Result` type (no expect
/// in `Drop`, review-verified) carries the rest.
#[test]
fn guard_drop_during_unwind_preserves_original_panic() {
    let outcome = std::panic::catch_unwind(|| {
        let _guard = Guard {
            id: Some("no-such-session-unwind-pin".to_string()),
        };
        panic!("original failure");
    });
    match outcome {
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic>");
            assert!(
                msg.contains("original failure"),
                "original panic payload preserved, got: {msg}"
            );
        }
        Ok(()) => panic!("unwind did not propagate"),
    }
}
