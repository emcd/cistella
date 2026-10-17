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
    use nix::sys::wait::{self, WaitStatus};
    use std::io::Read;
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
            let mut f = f;
            let mut buf = [0u8; 4096];
            let mut acc = Vec::new();
            let start = Instant::now();
            // Exit observed in the read phase counts as REAPED: a
            // WNOHANG poll reporting non-StillAlive reaps the
            // zombie, so a later waitpid would say ECHILD — which
            // is the reap, not a failure. Without this flag the
            // final stage mistakes ECHILD for an unreaped child
            // and fails a cleanly-exited session.
            let mut exited = false;
            while start.elapsed() < Duration::from_secs(30) {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        if String::from_utf8_lossy(&acc).contains("DONE") {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
                if wait::waitpid(child, Some(wait::WaitPidFlag::WNOHANG))
                    .is_ok_and(|s| s != WaitStatus::StillAlive)
                {
                    exited = true;
                    break;
                }
            }
            // Bounded final reap against the REMAINING window (never
            // a fresh wait). Already-observed exits skip the second
            // waitpid (ECHILD there IS the reap). A child that
            // outlives the session is killed and reports unattested
            // — forced-kill success never reads as success.
            let deadline = start + Duration::from_secs(30);
            let reaped = exited
                || loop {
                    match wait::waitpid(child, Some(wait::WaitPidFlag::WNOHANG)) {
                        Ok(WaitStatus::StillAlive) => {
                            if Instant::now() > deadline {
                                eprintln!(
                                    "enter pty child outlived its window; forcing SIGKILL (timeout, not success)"
                                );
                                let _ = nix::sys::signal::kill(
                                    child,
                                    nix::sys::signal::Signal::SIGKILL,
                                );
                                let _ = reap_bounded_pid(child, Duration::from_secs(10));
                                break false;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Ok(_) => break true,
                        Err(_) => break false,
                    }
                };
            let txt = String::from_utf8_lossy(&acc).to_string();
            eprintln!("enter pty acc: {txt}");
            reaped && (txt.contains("30 100") || (txt.contains("DONE") && !txt.contains("0 0")))
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
