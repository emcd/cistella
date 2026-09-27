//! Hook-launch diagnostics delivery (task 3.2).
//!
//! The guest forwards the hook diagnostics write-end into the
//! container at its natural fd number on the singular path
//! (duped onto [`PLURAL_DIAG_FD`] pre-exec on the plural path): argv verification
//! (re-checking the prepare-time binding at composition), crun
//! detection for exact-fd preservation, receive-side CLOEXEC
//! clearing, and `--diagnostics-fd` insertion. Lives apart from
//! the Podman backend so the backend file stays under its line
//! budget; called once per hooked launch (no shared mutable fd
//! state, so parallel hooked launches need no lock).

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::process::Command;

use nix::fcntl::{FcntlArg, FdFlag};

use crate::error::{CistellaError, Result};
use crate::transport::{
    PLURAL_DIAG_FD, PreserveStrategy, assert_plural_inheritable_only, resolve_preserve_strategy,
};

/// Prepares diagnostics forwarding for a hooked launch:
/// verifies argv[0] names the staged wrapper (re-checking the
/// prepare-time binding at composition), requires the crun
/// runtime (exact-fd preservation is crun-only per podman docs;
/// the plural path keeps the same gate in Phase 1 — runc
/// characterization is a pending spike, not an allowance),
/// clears receive-side CLOEXEC so the forked podman client
/// carries the fd, and inserts `--diagnostics-fd={n}`
/// immediately after the wrapper executable: the natural number
/// on the singular path, [`PLURAL_DIAG_FD`] on the plural path
/// (the caller dups there pre-exec). Returns the natural
/// number for the exec argv and the plural precondition. The
/// guest copy drops in the caller after spawn, so the framework
/// observes EOF once the wrapper seals at exec.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on wrapper mismatch or a
/// non-crun runtime, and `CistellaError::Runtime` on
/// detection/fcntl failure.
pub fn prepare_diagnostics_hook(
    diag: &OwnedFd,
    argv: &mut Vec<String>,
    strategy: PreserveStrategy,
) -> Result<RawFd> {
    use crate::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    match argv.first() {
        Some(first) if first == STAGED_WRAPPER_GUEST_PATH => {}
        _ => {
            return Err(CistellaError::Contract(
                "hook launch argv must start with the staged wrapper".to_string(),
            ));
        }
    }
    let runtime = oci_runtime_name()?;
    if runtime != "crun" {
        return Err(CistellaError::Contract(format!(
            "hook diagnostics require the crun OCI runtime, found {runtime}"
        )));
    }
    let raw = diag.as_raw_fd();
    let current = nix::fcntl::fcntl(raw, FcntlArg::F_GETFD)
        .map_err(|e| CistellaError::Runtime(format!("diagnostics fcntl: {e}")))?;
    let cleared = FdFlag::from_bits_retain(current) & !FdFlag::FD_CLOEXEC;
    nix::fcntl::fcntl(raw, FcntlArg::F_SETFD(cleared))
        .map_err(|e| CistellaError::Runtime(format!("diagnostics fcntl: {e}")))?;
    let number = match strategy {
        PreserveStrategy::Singular => raw,
        PreserveStrategy::Plural => PLURAL_DIAG_FD,
    };
    argv.insert(1, format!("--diagnostics-fd={number}"));
    Ok(raw)
}

/// Names the OCI runtime backing local podman (`podman info`
/// JSON, `host.ociRuntime.name`). Hooked launches need exact-fd
/// preservation (crun-only per podman docs); anything else
/// refuses typed before spawn.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on command/parse failure
/// and `CistellaError::Contract` on a non-crun runtime.
fn oci_runtime_name() -> Result<String> {
    let out = Command::new("podman")
        .args(["info", "--format", "json"])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman info failed: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "podman info non-zero: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CistellaError::Runtime(format!("podman info json parse: {e}")))?;
    let name = value
        .get("host")
        .and_then(|host| host.get("ociRuntime"))
        .and_then(|runtime| runtime.get("name"))
        .and_then(|name| name.as_str())
        .unwrap_or("")
        .to_string();
    Ok(name)
}

/// Process-wide hook-spawn critical section (tier-1 pushback):
/// the fd table is process-wide, so concurrent hooked spawns —
/// in this backend or another instance in the same process —
/// must serialize their census-to-spawn windows, or one
/// launch could census while another clears its diagnostics
/// fd (spurious refusal at best, cross-launch inheritance at
/// worst). Plain spawns never clear CLOEXEC and stay outside.
/// The held guard is returned with the bundle so the section
/// spans through spawn; the per-launch assertion remains the
/// tripwire for out-of-section inheritable fds (fail-closed,
/// never silent).
pub fn lock_hook_spawn() -> Result<std::sync::MutexGuard<'static, ()>> {
    static HOOK_SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    HOOK_SPAWN_LOCK
        .lock()
        .map_err(|_| CistellaError::Contract("hook spawn lock poisoned".to_string()))
}

/// Resolved hook preservation for one launch: the strategy
/// plus the natural diagnostics fd number for the exec argv
/// (or nothing without a diagnostics fd), with the hook-spawn
/// lock held through the caller's spawn.
pub struct HookedPreservation {
    /// Strategy and natural fd, or `None` without diagnostics.
    pub hooked: Option<(PreserveStrategy, RawFd)>,
    /// Held hook-spawn lock (drop after spawn releases).
    pub guard: std::sync::MutexGuard<'static, ()>,
}

/// Resolves the preservation strategy and prepares diagnostics
/// forwarding for one hooked launch (or nothing without a
/// diagnostics fd): strategy resolution first (the prep inserts
/// the strategy's diagnostics-fd number — natural on singular,
/// [`PLURAL_DIAG_FD`] on plural), then the plural precondition
/// asserted every launch pre-spawn. The returned guard holds
/// the hook-spawn lock through the caller's spawn (drop after
/// spawn releases the section; await never holds it).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on detection/census failure
/// and `CistellaError::Contract` on runtime, wrapper,
/// precondition, or lock refusal.
pub fn resolve_hooked_preservation(
    diagnostics: Option<&OwnedFd>,
    argv: &mut Vec<String>,
) -> Result<HookedPreservation> {
    let guard = lock_hook_spawn()?;
    let Some(diag) = diagnostics else {
        return Ok(HookedPreservation {
            hooked: None,
            guard,
        });
    };
    let resolved = resolve_preserve_strategy()?;
    let natural = prepare_diagnostics_hook(diag, argv, resolved)?;
    if resolved == PreserveStrategy::Plural {
        assert_plural_inheritable_only(natural)?;
    }
    Ok(HookedPreservation {
        hooked: Some((resolved, natural)),
        guard,
    })
}

/// Collapses the diagnostics write-end onto [`PLURAL_DIAG_FD`]
/// child-side (parent fd 3 may be live) so `--preserve-fds=1`
/// forwards exactly `{0,1,2,3}`. Runs in the spawn pre-exec
/// hook; the pre-spawn assertion proved nothing else
/// inheritable. Only async-signal-safe calls (dup2, close).
pub fn dup_plural_diag(diag: RawFd) -> std::io::Result<()> {
    if diag != PLURAL_DIAG_FD {
        nix::unistd::dup2(diag, PLURAL_DIAG_FD).map_err(std::io::Error::from)?;
        nix::unistd::close(diag).map_err(std::io::Error::from)?;
    }
    Ok(())
}
