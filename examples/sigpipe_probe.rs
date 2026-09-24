//! `sigpipe_probe` — isolated SIG_DFL closed-pipe reproduction.
//!
//! The regression test for per-write SIGPIPE masking must run with a
//! default SIGPIPE disposition, which is process-global and must
//! never be touched inside the shared multithreaded test runner.
//! This tiny example binary owns its whole process: it sets SIG_DFL,
//! writes one frame to a dropped socketpair end through the public
//! `write_frame` path, and exits 0 only on typed EPIPE plus survival
//! (a delayed SIGPIPE delivery would kill it first).
//!
//! Cargo autodiscovers (lands at `target/<profile>/examples/`);
//! out of `package.include`.

use std::os::unix::net::UnixStream;

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

fn main() -> std::process::ExitCode {
    unsafe {
        let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        if sigaction(Signal::SIGPIPE, &dfl).is_err() {
            eprintln!("PROBE_FAIL: cannot set SIG_DFL");
            return std::process::ExitCode::from(1);
        }
    }
    let (mut writer, reader) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("PROBE_FAIL: socketpair: {e}");
            return std::process::ExitCode::from(1);
        }
    };
    drop(reader);
    match cistella::framework::protocol::write_frame(&mut writer, b"hello", 1024) {
        Err(e) if e.to_string().contains("frame write") => {
            println!("PROBE_OK: {e}");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("PROBE_FAIL: wrong error: {e}");
            std::process::ExitCode::from(1)
        }
        Ok(()) => {
            eprintln!("PROBE_FAIL: write to closed pipe succeeded");
            std::process::ExitCode::from(1)
        }
    }
}
