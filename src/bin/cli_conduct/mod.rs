//! The `conduct` verb for the `cistella` binary: session hosting
//! orchestration (mint, install, start, exec, teardown) plus its
//! helpers.
//!
//! Bin-crate module (declared in `bin/cistella.rs`, NOT part of the library):
//! moved verbatim from `main.rs`, no logic changes.

/// Returns the cwd as a string for the `--session-directory` host default.
fn cwd_string() -> Result<String, cistella::error::CistellaError> {
    std::env::current_dir()
        .map_err(|e| {
            cistella::error::CistellaError::Runtime(format!("cwd: {e}"))
        })
        .map(|p| p.to_string_lossy().to_string())
}

/// Returns the default identity label (invoking user, else `default`).
fn default_identity() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    if !user.is_empty() {
        return user;
    }
    let out = std::process::Command::new("id")
        .arg("-un")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

// Session lifecycle and retirement live in submodules; the hub
// re-exports every name so existing paths keep working.
mod retire;
mod session;
pub(crate) use self::retire::retire_and_rehost;
pub(crate) use self::session::conduct_session;

/// Test hook: milliseconds to stall between install and start, polling for
/// signals, opening a deterministic signal-during-startup window.
fn start_delay_ms() -> u64 {
    std::env::var("CISTELLA_CONDUCT_START_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Test hook: path of a release marker file. When set, the
/// install/start stall polls for that file instead of sleeping
/// a fixed duration: release is the test creating the file,
/// abort is a pending signal. This is explicitly a
/// test-only synchronization hook next to
/// `start_delay_ms` — a bounded-interval existence check plus
/// a per-iteration signal check, with no overall timeout and
/// no bearing on the enter/readiness policy. Absence of the
/// file never advances the session; the test's own bound
/// fails the test instead.
fn start_gate() -> Option<std::path::PathBuf> {
    std::env::var("CISTELLA_CONDUCT_START_GATE")
        .ok()
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}
