//! Forkpty helpers for the test harness: bounded pid reaps,
//! failure-path cleanup, and the enter-via-pty session probe.
//!
//! Split from `helpers.rs` (line-count hook). `helpers.rs`
//! re-exports this module so existing `use super::helpers::*`
//! consumers are unaffected.

use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::time::{Duration, Instant};

use super::helpers::bin;

/// Bounded reap of a forkpty child by pid: WNOHANG loop against
/// the bound after SIGKILL. True = reaped. A pid that will not reap
/// is wedged; the caller reports loudly — bounded, never hangs.
/// Public so forkpty-based tests share the one bounded reap.
pub fn reap_bounded_pid(child: nix::unistd::Pid, timeout: Duration) -> bool {
    use nix::sys::wait::{WaitPidFlag, WaitStatus};
    let start = Instant::now();
    loop {
        match nix::sys::wait::waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if start.elapsed() > timeout {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(_) => return true,
            Err(_) => return false,
        }
    }
}

/// Best-effort kill + bounded reap of an owned forkpty child on
/// paths that already failed (setup refusal, unattested runs).
/// The failure stands; this only prevents a leaked child from
/// interfering with sibling tests. An unreaped child is logged,
/// never silently dropped. Public so forkpty-based tests share
/// the one cleanup path.
pub fn kill_reap_pty_child(child: nix::unistd::Pid) {
    let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
    if !reap_bounded_pid(child, Duration::from_secs(10)) {
        eprintln!("pty child {child} would not reap after SIGKILL");
    }
}

pub fn enter_stty_via_pty(home: &str, worktree: &str) -> bool {
    use nix::pty::{ForkptyResult, Winsize, forkpty};
    let winsize = Winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    match unsafe { forkpty(Some(&winsize), None) } {
        Ok(ForkptyResult::Parent { child, master }) => {
            let f = std::fs::File::from(master);
            // Nonblocking master: the 30s window below must bound
            // controller progress, and a blocking `read` would sit
            // past it on a silent child. Setup failure surfaces
            // (false + diagnostic), never degrades silently — and
            // the forkpty child is killed first, never leaked to
            // interfere with sibling tests.
            if let Ok(flags) = nix::fcntl::fcntl(f.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL) {
                let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
                oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                if nix::fcntl::fcntl(f.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags)).is_err()
                {
                    eprintln!("enter pty: O_NONBLOCK setup refused");
                    kill_reap_pty_child(child);
                    return false;
                }
            } else {
                eprintln!("enter pty: F_GETFL refused");
                kill_reap_pty_child(child);
                return false;
            }
            let f = f;
            let start = Instant::now();
            // Bounded capture owns the read loop (facts only):
            // drain to a terminal observation with the settle
            // predicate on DONE. The shared finalization below
            // carries the first actually consumed disposition
            // into the verdict; the verdict itself is unchanged
            // in shape, now decided on complete facts.
            let outcome = super::capture::capture_master(
                &f,
                child,
                Duration::from_secs(30),
                Duration::from_secs(5),
                super::capture::CAPTURE_CAP,
                |bytes| String::from_utf8_lossy(bytes).contains("DONE"),
            );
            let deadline = start + Duration::from_secs(30);
            let fin = super::capture::finalize_capture(child, "enter pty", &outcome, deadline);
            let txt = String::from_utf8_lossy(&outcome.bytes).to_string();
            let final_exit = super::capture::ms_since(outcome.phases.started, fin.exit_observed);
            eprintln!(
                "enter pty acc: {}",
                super::capture::excerpt_capped(&outcome.bytes)
            );
            eprintln!(
                "enter pty capture: end={:?} reads={} attempts={} retained={} matched={} truncated={} reaped={} status={:?} final={:?} forced_kill={} phases={} final_exit={} hits={:?}",
                outcome.end,
                outcome.reads,
                outcome.attempts,
                outcome.bytes.len(),
                outcome.matched,
                outcome.truncated,
                outcome.child_reaped,
                outcome.disposition,
                fin.disposition,
                fin.forced_kill,
                super::capture::phase_offsets(&outcome.phases),
                final_exit,
                outcome.refusal_hits
            );
            // Verdict admits a clean end of stream (EOF, or EIO
            // observing no open slave) with a successfully
            // exited child and the expected content. Probe
            // contract: the harness under test never reopens
            // the slave once closed, so a closed slave means no
            // further output from this probe; a reopened slave
            // is outside this contract. Anything else — capture
            // error or timeout, truncation, an unknown or
            // unsuccessful disposition (including a disposition
            // first observed here, in finalization), a missing
            // marker — fails loud instead of passing on a
            // partial capture.
            let end_ok = matches!(
                outcome.end,
                super::capture::CaptureEnd::Eof | super::capture::CaptureEnd::PtyEio { .. }
            );
            let content_ok =
                txt.contains("30 100") || (txt.contains("DONE") && !txt.contains("0 0"));
            super::capture::verdict_ready(end_ok, &fin, content_ok)
        }
        Ok(ForkptyResult::Child) => {
            use std::os::unix::process::CommandExt;
            let home = home.to_string();
            let worktree = worktree.to_string();
            let e = Command::new(bin())
                .args([
                    "enter",
                    "--directory",
                    &worktree,
                    "--",
                    "sh",
                    "-c",
                    "stty size; echo DONE",
                ])
                .env("HOME", home)
                .env("TERM", "xterm-ghostty")
                .exec();
            eprintln!("cistella enter {e:?}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("forkpty {e}");
            false
        }
    }
}
