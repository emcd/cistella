//! Conduct-level signal traps backed by a framework cancel flag.
//!
//! Flag-only handlers record SIGHUP/SIGTERM before any residue
//! exists; the harnessed child resets to default in `pre_exec` so it
//! still dies with the pane. Moved here from the CLI entry so
//! isolator mechanics (await polling) share one cancellation source
//! with conduct orchestration.

use std::sync::atomic::{AtomicBool, Ordering};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

use crate::framework::contract::CancelFlag;

/// Set when `conduct` receives `SIGHUP`.
static GOT_HUP: AtomicBool = AtomicBool::new(false);
/// Set when `conduct` receives `SIGTERM`.
static GOT_TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_conduct_signal(signal: nix::libc::c_int) {
    if signal == nix::libc::SIGHUP {
        GOT_HUP.store(true, Ordering::SeqCst);
        conduct_cancel().cancel_with(1);
    } else if signal == nix::libc::SIGTERM {
        GOT_TERM.store(true, Ordering::SeqCst);
        conduct_cancel().cancel_with(15);
    }
}

/// Process-wide conduct cancellation flag.
pub fn conduct_cancel() -> &'static CancelFlag {
    static FLAG: CancelFlag = CancelFlag::new();
    &FLAG
}

/// Installs the conduct-level SIGHUP/SIGTERM traps (flag-only handlers).
///
/// Call before the lock is acquired or any residue is created, so a
/// signal during startup tears down instead of taking the default
/// action. Safe to call repeatedly: reinstalling the same flag-only
/// handler is a no-op for flag storage, so callers need no guard.
pub fn install_conduct_handlers() {
    unsafe {
        let action = SigAction::new(
            SigHandler::Handler(on_conduct_signal),
            SaFlags::empty(),
            SigSet::empty(),
        );
        let _ = sigaction(Signal::SIGHUP, &action);
        let _ = sigaction(Signal::SIGTERM, &action);
    }
}

/// Returns the pending conduct-level signal, if SIGHUP/SIGTERM arrived.
#[must_use]
pub fn pending_signal() -> Option<i32> {
    if GOT_HUP.load(Ordering::SeqCst) {
        Some(1)
    } else if GOT_TERM.load(Ordering::SeqCst) {
        Some(15)
    } else {
        None
    }
}

/// True once SIGHUP arrived.
#[must_use]
pub fn got_hup() -> bool {
    GOT_HUP.load(Ordering::SeqCst)
}

/// True once SIGTERM arrived.
#[must_use]
pub fn got_term() -> bool {
    GOT_TERM.load(Ordering::SeqCst)
}
