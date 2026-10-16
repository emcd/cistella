//! ConductTaps drain pins: completion and held-open timeout, no systemd.
//!
//! Pins the timeout-without-join behavior `finalize` promises: a
//! descendant-held pipe must surface as a labeled partial-timeout,
//! never hang the test in a reaping join. Both cases drive the
//! PRODUCTION drain worker (`run_tap_drain` via `ConductTaps::for_reader`),
//! so they pin shipped behavior, not a test double. Sockets stand in
//! for pipes (blocking `Read`, holdable open, no subprocess needed).

use std::os::unix::net::UnixStream;

use super::helpers::*;

/// Closed writer → clean EOF: finalize attests completion and the
/// retained tail carries every written byte.
#[test]
fn tap_drain_completion_attests_and_retains() {
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    let mut taps = ConductTaps::for_reader(reader);
    std::io::Write::write_all(&mut writer, b"conduct teardown line\n").expect("write tap");
    drop(writer);
    let tap = taps.finalize(std::time::Duration::from_secs(10));
    assert!(tap.drain_clean, "clean EOF attested: {}", tap.drain_note);
    assert!(
        tap.stderr.contains("conduct teardown line"),
        "retained tail carries written bytes: {}",
        tap.stderr
    );
}

/// Held-open writer (no EOF, descendant-holding-pipe shape) →
/// finalize RETURNS with a labeled partial timeout instead of hanging
/// in the reaping join. The elapsed bound is the pin: pre-fix code
/// joined unconditionally after `recv_timeout` and would block here
/// until EOF that never comes.
#[test]
fn tap_drain_held_open_returns_partial_timeout() {
    let (reader, writer) = UnixStream::pair().expect("socket pair");
    // Writer deliberately held open: no bytes, no EOF — the drain
    // worker blocks in `read` exactly like a descendant-held pipe.
    let mut taps = ConductTaps::for_reader(reader);
    let start = std::time::Instant::now();
    let tap = taps.finalize(std::time::Duration::from_millis(200));
    let elapsed = start.elapsed();
    // `writer` stays bound (pipe open, no EOF) through finalize and
    // the asserts below; dropped at scope end.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "finalize returned promptly on held-open pipe, took {elapsed:?}"
    );
    assert!(
        !tap.drain_clean,
        "held-open drain is not clean evidence: {}",
        tap.drain_note
    );
    assert!(
        tap.drain_note.contains("not complete"),
        "timeout labeled as unproven partial: {}",
        tap.drain_note
    );
    drop(writer);
}
