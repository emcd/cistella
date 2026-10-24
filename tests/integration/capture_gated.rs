//! Synchronized post-closure controls for the shared bounded
//! capture: gate-ordered exits proving the finalizer carries
//! the disposition into the verdict, plus the PTY early-close
//! admission pin. Split from `capture_tests.rs` at the
//! file-size limit; no timing, no descendants, every wait
//! bounded.

use std::os::unix::io::IntoRawFd;
use std::time::{Duration, Instant};

use super::capture::*;
use super::capture_tests::{close_unrelated_fds, gate_wait, set_nonblocking};

/// One synchronized post-closure round for the R1 controls:
/// the direct child writes `marker`, closes its writer (EOF
/// while it stays alive), waits on the gate, then exits per
/// `plan`. The parent captures, asserts the capture-time
/// premise (Unknown, unreaped — the child cannot exit before
/// the gate release, so a stalled schedule cannot sneak past
/// the repaired handoff), releases the gate, finalizes, and
/// returns both records for the verdict. No descendants, no
/// forgotten writers, every wait bounded.
#[derive(Debug, Clone, Copy)]
enum GatedExit {
    Status(i32),
    Signal(i32),
}

fn gated_post_closure_round(
    marker: &'static [u8],
    marker_text: &str,
    plan: GatedExit,
    label: &str,
) -> (CaptureOutcome, Finalization) {
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let (gate_read, gate_write) = nix::unistd::pipe().expect("pipe");
    match unsafe { nix::unistd::fork().expect("fork") } {
        nix::unistd::ForkResult::Parent { child } => {
            drop(write);
            drop(gate_read);
            let file = std::fs::File::from(read);
            set_nonblocking(&file);
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(2),
                CAPTURE_CAP,
                |bytes| String::from_utf8_lossy(bytes).contains(marker_text),
            );
            assert_eq!(outcome.bytes, marker, "marker bytes complete");
            assert!(outcome.matched, "settle predicate fires");
            assert_eq!(outcome.end, CaptureEnd::Eof);
            // Capture-time premise: the child is still alive and
            // unreaped (it is gate-blocked) — stopping here
            // would refuse a clean run, which is exactly the
            // handoff the finalizer repairs.
            assert_eq!(
                outcome.disposition,
                ChildDisposition::Unknown,
                "capture-time disposition is Unknown while the child is gate-blocked"
            );
            assert!(
                !outcome.child_reaped,
                "capture-time child is unreaped while gate-blocked"
            );
            assert!(
                outcome.phases.closed_at.is_some(),
                "early closure is observed, not assumed"
            );
            // Release, then finalize exactly as the call sites
            // use it: bounded wait, first-consumption
            // disposition, forced-kill accounting.
            let wrote = nix::unistd::write(&gate_write, &[1u8]);
            assert!(wrote.is_ok(), "gate release delivers");
            drop(gate_write);
            let fin = finalize_capture(
                child,
                label,
                &outcome,
                Instant::now() + Duration::from_secs(10),
            );
            (outcome, fin)
        }
        nix::unistd::ForkResult::Child => {
            unsafe { libc::close(gate_write.into_raw_fd()) };
            let gate = gate_read.into_raw_fd();
            unsafe { libc::close(read.into_raw_fd()) };
            let w = write.into_raw_fd();
            // A gate-blocked child outlives the capture
            // window: shed every inherited sibling fd first
            // so this block cannot hold another test's EOF
            // open.
            close_unrelated_fds(&[w, gate]);
            let wrote = unsafe {
                libc::write(
                    w,
                    marker.as_ptr() as *const std::os::raw::c_void,
                    marker.len() as libc::size_t,
                )
            };
            unsafe { libc::close(w) };
            if wrote != marker.len() as isize {
                unsafe { libc::_exit(2) };
            }
            // Stay alive past the closure so capture observes
            // EOF while the child is still unreaped (bounded:
            // a dead parent surfaces as exit 2, never a hang).
            if !gate_wait(gate, Duration::from_secs(15)) {
                unsafe { libc::_exit(2) };
            }
            unsafe { libc::close(gate) };
            match plan {
                GatedExit::Status(code) => unsafe { libc::_exit(code) },
                GatedExit::Signal(signal) => unsafe {
                    libc::raise(signal);
                    libc::_exit(3);
                },
            }
        }
    }
}

#[test]
fn capture_finalize_carries_post_closure_exit() {
    // R1 integrated path: closure first while the child stays
    // gate-blocked (capture-time Unknown/unreaped asserted in
    // the round), then the child exits 0. Finalize must carry
    // the actual disposition into the verdict inputs, and the
    // call-site end policy admits the clean EOF.
    let (outcome, fin) = gated_post_closure_round(
        b"POST-CLOSE\n",
        "POST-CLOSE",
        GatedExit::Status(0),
        "post-close",
    );
    assert!(fin.reaped, "final reap converges");
    assert!(!fin.forced_kill, "no kill needed for a clean exit");
    assert_eq!(
        fin.disposition,
        ChildDisposition::Exited(0),
        "post-closure exit is carried, not left Unknown"
    );
    let end_ok = matches!(outcome.end, CaptureEnd::Eof | CaptureEnd::PtyEio { .. });
    assert!(
        verdict_ready(end_ok, &fin, outcome.matched),
        "clean closure plus clean exit verifies"
    );
}

#[test]
fn capture_finalize_reports_nonzero_exit() {
    // Same synchronized route with a nonzero exit: the
    // disposition distinguishes it, and the shared verdict
    // refuses it even with a clean end and content.
    let (outcome, fin) = gated_post_closure_round(
        b"POST-CLOSE\n",
        "POST-CLOSE",
        GatedExit::Status(3),
        "post-close-nonzero",
    );
    assert!(fin.reaped, "final reap converges");
    assert_eq!(fin.disposition, ChildDisposition::Exited(3));
    let end_ok = matches!(outcome.end, CaptureEnd::Eof | CaptureEnd::PtyEio { .. });
    assert!(
        !verdict_ready(end_ok, &fin, true),
        "nonzero exit never verifies, even with clean end and content"
    );
}

#[test]
fn capture_finalize_reports_signal_death() {
    // Same synchronized route with a signal death: the
    // disposition distinguishes it from an exit code, and the
    // shared verdict refuses it.
    let (outcome, fin) = gated_post_closure_round(
        b"POST-CLOSE\n",
        "POST-CLOSE",
        GatedExit::Signal(libc::SIGTERM),
        "post-close-signal",
    );
    assert!(fin.reaped, "final reap converges");
    assert_eq!(
        fin.disposition,
        ChildDisposition::Signaled(libc::SIGTERM),
        "signal death recorded, not an exit code"
    );
    let end_ok = matches!(outcome.end, CaptureEnd::Eof | CaptureEnd::PtyEio { .. });
    assert!(
        !verdict_ready(end_ok, &fin, true),
        "signaled child never verifies, even with clean end and content"
    );
}

#[test]
fn capture_pty_early_close_admitted_after_clean_exit() {
    // The disputed R1 ordering pinned on the PTY route (no
    // pipe stand-in): the forked child writes, closes its
    // slave, and stays gate-blocked alive, so the master
    // observes EIO while the child is not yet waitable. The
    // capture-time record keeps the early closure (PtyEio,
    // unreaped, Unknown, closed_at set — never rewritten as
    // clean-at-capture); after the gated exit the final
    // disposition carries Exited(0) and the probe's end policy
    // admits it. Single owned direct child, no descendants, no
    // forgotten writers, every wait bounded.
    let (gate_read, gate_write) = nix::unistd::pipe().expect("pipe");
    match unsafe { nix::pty::forkpty(None, None).expect("forkpty") } {
        nix::pty::ForkptyResult::Parent { child, master } => {
            drop(gate_read);
            let file = std::fs::File::from(master);
            set_nonblocking(&file);
            let outcome = capture_master(
                &file,
                child,
                Duration::from_secs(10),
                Duration::from_secs(2),
                CAPTURE_CAP,
                |bytes| String::from_utf8_lossy(bytes).contains("PTY-EARLY-CLOSE"),
            );
            assert!(
                outcome.bytes == b"PTY-EARLY-CLOSE\n" || outcome.bytes == b"PTY-EARLY-CLOSE\r\n",
                "queued bytes complete (pty may translate LF to CRLF), got: {:?}",
                outcome.bytes
            );
            assert!(
                matches!(
                    outcome.end,
                    CaptureEnd::PtyEio {
                        child_reaped: false
                    }
                ),
                "early close reads as unreaped EIO, got: {:?}",
                outcome.end
            );
            assert_eq!(
                outcome.disposition,
                ChildDisposition::Unknown,
                "capture-time disposition is Unknown while the child is gate-blocked"
            );
            assert!(
                !outcome.child_reaped,
                "capture-time child is unreaped while gate-blocked"
            );
            assert!(
                outcome.phases.closed_at.is_some(),
                "early closure is observed, not assumed"
            );
            assert!(outcome.matched, "settle predicate fires");
            // The probe's end policy: EOF or EIO (observing no
            // open slave) with the probe's own children, which
            // never reopen the slave. A reopened slave is
            // outside this contract.
            let end_ok = matches!(outcome.end, CaptureEnd::Eof | CaptureEnd::PtyEio { .. });
            assert!(end_ok, "probe policy admits the early close");
            let wrote = nix::unistd::write(&gate_write, &[1u8]);
            assert!(wrote.is_ok(), "gate release delivers");
            drop(gate_write);
            let fin = finalize_capture(
                child,
                "pty-early-close",
                &outcome,
                Instant::now() + Duration::from_secs(10),
            );
            assert!(fin.reaped, "final reap converges");
            assert!(!fin.forced_kill, "no kill needed for a clean exit");
            assert_eq!(
                fin.disposition,
                ChildDisposition::Exited(0),
                "gate-released exit is carried, not left Unknown"
            );
            assert!(
                verdict_ready(end_ok, &fin, outcome.matched),
                "early EIO plus clean exit verifies under the probe contract"
            );
        }
        nix::pty::ForkptyResult::Child => {
            // Minimal post-fork body: close the unneeded gate
            // end, write, close the slave (fds 0/1/2), wait on
            // the gate (bounded), then exit clean.
            unsafe { libc::close(gate_write.into_raw_fd()) };
            let gate = gate_read.into_raw_fd();
            let message = b"PTY-EARLY-CLOSE\n";
            let wrote = unsafe {
                libc::write(
                    libc::STDOUT_FILENO,
                    message.as_ptr() as *const std::os::raw::c_void,
                    message.len() as libc::size_t,
                )
            };
            if wrote != message.len() as isize {
                unsafe { libc::_exit(2) };
            }
            // Shed every inherited sibling fd (including the
            // slave stdio just closed above): this gate block
            // outlives the capture window and must not hold
            // another test's EOF open.
            close_unrelated_fds(&[gate]);
            if !gate_wait(gate, Duration::from_secs(15)) {
                unsafe { libc::_exit(2) };
            }
            unsafe {
                libc::close(gate);
                libc::_exit(0);
            }
        }
    }
}
