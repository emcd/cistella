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

/// Deadline-bounded sink read of exactly `len` bytes: 10s per
/// read call, panic (not hang) past it. EOF before `len` ends
/// early (the caller asserts on the shortfall).
fn drain_sink_exact(
    mut sink: std::os::unix::net::UnixStream,
    len: usize,
) -> (std::os::unix::net::UnixStream, Vec<u8>) {
    use std::io::Read;
    sink.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("sink timeout armed");
    let mut got = Vec::new();
    let mut chunk = [0u8; 8192];
    while got.len() < len {
        match sink.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
            Err(e) => panic!("bounded sink completion failed: {e}"),
        }
    }
    (sink, got)
}

/// Lifetime-drain handoff pin (Tier-2 material finding on 29ae74a):
/// `read_conduct_id` leaves stdout O_NONBLOCK; the production
/// handoff must restore blocking BEFORE its one-shot copy, or the
/// first write gap ends the copy, drops the reader, and the child
/// dies SIGPIPE. This drives the ACTUAL helpers
/// (`restore_blocking` + `spawn_stdout_drain`, the exact
/// `spawn_conduct_tapped` sequence) with a CONSERVATION
/// handshake, not a sleep oracle: writer and sink are sized so no
/// pump thread is needed (64KB bulk into ~212KB default buffers);
/// bytes in the sink got there ONLY through the worker, so full
/// delivery proves the worker holds nothing. Its next read then
/// blocks whenever it is scheduled (blocking fd, empty pipe, no
/// EOF) — precisely what is claimed, no more: conservation
/// proves emptiness, not the instant the worker parks.
#[test]
fn stdout_drain_survives_quiet_gap_after_id_line() {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    // Simulate the reader's postcondition exactly: O_NONBLOCK set.
    // (No SO_RCVBUF games: unix(7) documents RcvBuf as void on
    // AF_UNIX — the handshake below needs no buffer tuning.)
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
    // Conservation: 64KB in; the worker alone moves it to the
    // sink. Phase 1 reads EXACTLY head+bulk: full arrival proves
    // the worker drained everything and holds nothing — the pipe
    // is empty while the writer stays idle.
    writer.write_all(&[b'k'; 65536]).expect("bulk");
    let (sink_reader, head) = drain_sink_exact(sink_reader, 8 + 65536);
    assert_eq!(
        head.len(),
        8 + 65536,
        "worker drained every bulk byte (pipe now provably empty)"
    );
    assert!(
        head.starts_with(b"LEFTOVER"),
        "leftover head delivered first"
    );
    writer.write_all(b"AFTER-GAP").expect("later write lands");
    drop(writer);
    // Bounded completion: AFTER-GAP is last and EOF follows the
    // dropped writer deterministically, so the join after is
    // prompt. Pre-fix code fails here instead: it exited on the
    // empty-pipe WouldBlock during the separator, EOF arrives
    // with no trailer, and the assertion names the shortfall.
    let (_, tail) = drain_sink_exact(sink_reader, 9);
    drain.join().expect("drain thread reaped");
    assert_eq!(
        head.len() + tail.len(),
        8 + 65536 + 9,
        "every byte delivered exactly once: head {} + tail {}",
        head.len(),
        tail.len()
    );
    assert_eq!(
        tail, b"AFTER-GAP",
        "post-gap write delivered last (no WouldBlock copy exit)"
    );
}

/// Negative control for the handoff (restore OMITTED).
/// Ordering by CONSTRUCTION, not scheduling: a small payload is
/// prequeued BEFORE the worker spawns (1KB fits any buffer, so
/// the write is bounded with no size assumptions) — the worker
/// cannot exit before consuming it, closing the opening race
/// where a first-scheduled worker WouldBlocks on an empty pipe
/// before the initial bulk is written. With the writer kept
/// alive (no EOF possible), the bounded join returning proves
/// WouldBlock exit (panic would be join Err); sink EOF right
/// after proves teardown with nothing in flight. No sleeps, no
/// EPIPE-vs-close race, no join hang on regression (done
/// channel bounds completion).
#[test]
fn stdout_drain_without_restore_loses_post_gap_write() {
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    let flags =
        nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).expect("F_GETFL");
    let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
    oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(reader.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags))
        .expect("O_NONBLOCK");
    let (sink_reader, sink_writer) = UnixStream::pair().expect("sink pair");
    // Prequeue BEFORE spawn: bounded 1KB write, fits any buffer.
    writer.write_all(&[b'k'; 1024]).expect("prequeued payload");
    // NOTE: no restore_blocking — the defect under test.
    let drain = spawn_stdout_drain(reader, b"LEFTOVER".to_vec(), sink_writer);
    // Full delivery observed (head + prequeue): the worker
    // consumed everything, so its next read hits the empty
    // nonblocking pipe and exits. Writer stays alive throughout.
    let (mut sink_reader, got) = drain_sink_exact(sink_reader, 8 + 1024);
    assert_eq!(got.len(), 8 + 1024, "bulk fully delivered first");
    assert!(
        got.starts_with(b"LEFTOVER"),
        "leftover head delivered first"
    );
    // Bounded completion: the join runs in a thread behind a
    // done channel, so a regression (worker never exits) fails
    // the test instead of hanging it.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drain.join().expect("worker exited on WouldBlock");
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("worker completion bounded");
    // Teardown completed with nothing more in flight: immediate
    // EOF, no trailer. (Production code would now SIGPIPE a
    // later conduct write — the mechanism this control exists
    // for — but asserting EPIPE here reintroduces the very
    // close-timing race Tier-1 caught; the exit proof above is
    // the deterministic part.)
    sink_reader
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("sink timeout armed");
    let mut eof = [0u8; 1];
    match sink_reader.read(&mut eof) {
        Ok(0) => {}
        other => panic!("sink closed with worker teardown, got: {other:?}"),
    }
    drop(writer);
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

/// Multibyte codepoint straddling the 8 KiB diagnostic cut: one
/// ASCII byte plus 4097 é's (U+00E9, 2 bytes each) = 8195 bytes,
/// so the 8192 cut lands INSIDE é #4096. The timeout path must
/// render partials WITHOUT panicking inside `str` indexing
/// (pre-fix `&text[..8192]` panicked on the split codepoint,
/// masking the timeout itself).
#[test]
fn multibyte_at_truncation_boundary_does_not_panic() {
    let mut cmd = std::process::Command::new("sh");
    // 1 + 4097×2 = 8195 bytes, then hang for the timeout.
    // `exec` replaces the shell with the sleeper so the timeout
    // kill reaps the direct child — no orphaned middleman
    // holding the pipe past the test.
    cmd.args([
        "-c",
        "printf 'X'; printf '\\303\\251%.0s' $(seq 1 4097); exec sleep 60",
    ]);
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

/// Held-writer pipe (write end held open, no EOF —
/// descendant-held shape): `wait_drain` returns false at its
/// deadline instead of hanging, and `finish_capture` assembles the
/// labeled `DrainTimeout` (never `Ok`). The holder drops after 2s
/// on a joined thread — no orphaned sleepers contaminating
/// concurrent runs; the 300ms deadline proves the bound while the
/// pipe is definitely still held.
#[test]
fn held_writer_pipe_drain_times_out_labeled() {
    use std::sync::{Arc, Mutex};
    let (reader, holder) = UnixStream::pair().expect("socket pair");
    let holder_thread = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        drop(holder);
    });
    let out_slot = Arc::new(Mutex::new(StreamCapture::empty()));
    let err_slot = Arc::new(Mutex::new(StreamCapture::empty()));
    let (tx, rx) = std::sync::mpsc::channel();
    let drain = drain_capped(reader, Arc::clone(&out_slot), tx);
    let start = std::time::Instant::now();
    let done = wait_drain(&rx, start + std::time::Duration::from_millis(300));
    assert!(!done, "held pipe not complete at the deadline");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "deadline honored promptly: {:?}",
        start.elapsed()
    );
    // Holder drops at 2s → EOF → worker completes → join prompt.
    // Then the assembly refuses Ok on the timed-out drain.
    holder_thread.join().expect("holder drops");
    drain.join().expect("drain thread reaped");
    let status = std::os::unix::process::ExitStatusExt::from_raw(0);
    let outcome = finish_capture(status, &out_slot, &err_slot, false, true);
    assert!(
        matches!(outcome, Err(BoundedRunError::DrainTimeout { .. })),
        "held pipe assembles as drain timeout, never Ok: {outcome:?}"
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

/// Single-deadline composition: exit, kill+reap, and drain share
/// ONE deadline instead of stacking fresh budgets past it. A
/// stuck child with a 22s deadline is killed at ~2s (22 − 20
/// reserve), proving the cleanup reserve was held back from the
/// exit phase — a naive full-deadline exit wait would run the
/// whole 22s before cleanup even starts.
#[test]
fn bounded_at_holds_one_deadline_for_exit_and_cleanup() {
    let child = std::process::Command::new("sleep")
        .arg("60")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sleep");
    let start = std::time::Instant::now();
    let outcome = wait_bounded_at(child, start + std::time::Duration::from_secs(22));
    let elapsed = start.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "kill fired at the reserved exit end (~2s), not instantly: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(22),
        "total stayed inside the single deadline (no stacked budgets): {elapsed:?}"
    );
    match outcome {
        Err(BoundedRunError::Timeout { reaped, .. }) => {
            assert!(reaped, "composition kill reaps: no zombie left behind")
        }
        other => panic!("stuck child reads as timeout: {other:?}"),
    }
}

/// Probe exit-status classification (Tier-2 round 2): exit 125 and
/// signal death are operational failures (`Broken`, stderr
/// retained), never missing-capability skips — only documented
/// absence codes (image-exists 1) read `Unavailable`. Statuses are
/// fabricated wait-status words (no subprocess needed):
/// exit N = N<<8, SIGKILL death = 9.
#[test]
fn probe_exit_classification_distinguishes_operational_failure() {
    use std::os::unix::process::ExitStatusExt;
    let exit = |code: u32| ExitStatusExt::from_raw((code << 8) as i32);
    let killed = ExitStatusExt::from_raw(9);
    // image-exists table: 0 present, 1 absent, 125 + signal broken.
    assert!(matches!(
        classify_probe_exit("podman image exists", exit(0), "", &[1]),
        Probe::Available
    ));
    assert!(matches!(
        classify_probe_exit("podman image exists", exit(1), "absent", &[1]),
        Probe::Unavailable(_)
    ));
    match classify_probe_exit("podman image exists", exit(125), "Error: daemon", &[1]) {
        Probe::Broken(detail) => assert!(
            detail.contains("daemon"),
            "stderr retained in Broken: {detail}"
        ),
        other => panic!("exit 125 is operational failure, never a skip: {other:?}"),
    }
    assert!(
        matches!(
            classify_probe_exit("podman image exists", killed, "", &[1]),
            Probe::Broken(_)
        ),
        "signal death is never a missing capability"
    );
    // podman-info table: no documented absence codes — any
    // nonzero from a present binary is operational.
    assert!(matches!(
        classify_probe_exit("podman info", exit(0), "", &[]),
        Probe::Available
    ));
    assert!(
        matches!(
            classify_probe_exit("podman info", exit(1), "", &[]),
            Probe::Broken(_)
        ),
        "unlisted nonzero is operational failure, never a skip"
    );
    assert!(
        matches!(
            classify_probe_exit("podman info", exit(125), "", &[]),
            Probe::Broken(_)
        ),
        "exit 125 is operational failure"
    );
    assert!(
        matches!(
            classify_probe_exit("podman info", killed, "", &[]),
            Probe::Broken(_)
        ),
        "signal death is never a missing capability"
    );
}

/// Injected failure through the Guard Drop seam (Tier-2 round 3):
/// `guard_drop_inner` takes the HOME lookup as a `Result`, so the
/// HOME-missing path is pinnable WITHOUT process-global env
/// mutation (unsetting HOME would race parallel tests). Every
/// branch returns normally and logs — a cleanup failure during
/// an assertion unwind must never panic (double-panic aborts and
/// masks the original evidence). A terminate-timeout DURING
/// unwind stays unforgable without a fault seam, disclosed; the
/// `Result` type plus this pin plus the unwind pin carry it.
#[test]
fn guard_drop_inner_failure_seams_return_without_panic() {
    // HOME-missing path, with and without an armed id.
    guard_drop_inner(
        Err(std::env::VarError::NotPresent),
        Some("any-id".to_string()),
    );
    guard_drop_inner(Err(std::env::VarError::NotPresent), None);
    // Full terminate path with a bogus home/id (fast failure,
    // exercises the logging branch to completion).
    guard_drop_inner(
        Ok("no-such-home-guard-pin".to_string()),
        Some("no-such-session-inner-pin".to_string()),
    );
}

/// Held-descendant pipe through the ACTUAL `wait_bounded`
/// integration (not only the `wait_drain` helper): the shell exits
/// at once, its backgrounded sleeper holds stdout, and the
/// post-exit drain caps at exit+allowance (~10s), NOT the ~24s
/// outer budget. The sleeper's pid is captured to a file BEFORE
/// the wait so the test retains control: SIGKILL to the exact pid
/// plus an absence poll after the assertion — no orphaned
/// sleepers contaminating concurrent runs.
#[test]
fn held_descendant_pipe_caps_drain_at_exit_plus_allowance() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let pid_dir = tempfile::TempDir::new().expect("pid dir");
    let pidfile = pid_dir.path().join("sleeper.pid");
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", &format!("sleep 30 & echo $! > {}", pidfile.display())]);
    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn shell");
    let start = std::time::Instant::now();
    let outcome = wait_bounded(child, std::time::Duration::from_secs(2));
    let elapsed = start.elapsed();
    // Retain control FIRST (before asserting): exact pid from the
    // file (never a pattern kill), SIGKILL, then poll for absence.
    // A failed assertion below must not orphan the sleeper.
    let pid_text = std::fs::read_to_string(&pidfile).expect("sleeper pid recorded");
    let pid = Pid::from_raw(pid_text.trim().parse::<i32>().expect("pid parses"));
    kill(pid, Signal::SIGKILL).expect("sleeper signaled");
    let gone_start = std::time::Instant::now();
    loop {
        match kill(pid, None) {
            Err(nix::errno::Errno::ESRCH) => break,
            _ => {
                assert!(
                    gone_start.elapsed() < std::time::Duration::from_secs(5),
                    "sleeper reaped after SIGKILL"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    assert!(
        elapsed >= std::time::Duration::from_secs(8),
        "drain waited out the post-exit cap (~10s), not an early return: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "drain capped at exit+allowance, not the outer budget: {elapsed:?}"
    );
    assert!(
        matches!(outcome, Err(BoundedRunError::DrainTimeout { .. })),
        "held pipe reads as drain timeout: {outcome:?}"
    );
}

/// Forced reap-failure in the shared overflow-cleanup branch
/// (Tier-2 round 4): the cleanup step is injected, so the pin
/// passes a failing cleanup (`|_| false`) with a live child and
/// poisoned slot — deterministically exercising the
/// `Timeout{reaped:false}` assembly with partials, promptly.
/// (Neither SIGSTOP nor pre-reaping forces this: SIGKILL still
/// reaps stopped processes, and Rust caches a waited child's
/// status so `try_wait` reports it. Only D-state wedges fail
/// production cleanup — unforgable in a fast pin, hence the
/// injected seam.) The live child is killed+reaped after: no
/// orphans.
#[test]
fn overflow_cleanup_reap_failure_reports_without_deadlock() {
    use std::sync::{Arc, Mutex};
    let mut child = std::process::Command::new("sleep")
        .arg("300")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sleep");
    let out_slot = Arc::new(Mutex::new(StreamCapture {
        buf: b"partial".to_vec(),
        overflow: true,
        error: None,
    }));
    let err_slot = Arc::new(Mutex::new(StreamCapture::empty()));
    let start = std::time::Instant::now();
    let outcome = overflow_cleanup(&mut child, &out_slot, &err_slot, start, |_| false);
    let elapsed = start.elapsed();
    // Owned cleanup FIRST (before asserting): a failed assertion
    // below must not strand the live child. SIGKILL now; the
    // reap assert stays last (a missed reap leaves only a
    // kernel-reaped zombie, never an orphan).
    let _ = child.kill();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "forced reap-failure returned (no deadlock), took {elapsed:?}"
    );
    match outcome {
        Err(BoundedRunError::Timeout {
            reaped,
            partial_stdout,
            ..
        }) => {
            assert!(!reaped, "unreaped kill reports reaped:false");
            assert!(
                partial_stdout.contains("partial"),
                "retained bytes kept as triage: {partial_stdout:?}"
            );
        }
        other => panic!("forced reap-failure reads as unreaped timeout: {other:?}"),
    }
    assert!(
        reap_bounded(&mut child, CHILD_REAP_TIMEOUT).is_some(),
        "pin child reaps after SIGKILL"
    );
}
