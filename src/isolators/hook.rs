//! Hook-launch diagnostics delivery (task 3.2).
//!
//! The guest forwards the hook diagnostics write-end into the
//! container at its natural fd number: argv verification
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

/// Prepares diagnostics forwarding for a hooked launch:
/// verifies argv[0] names the staged wrapper (re-checking the
/// prepare-time binding at composition), requires the crun
/// runtime for exact-fd preservation, clears receive-side
/// CLOEXEC so the forked podman client carries the fd, and
/// inserts `--diagnostics-fd={n}` immediately after the wrapper
/// executable. Returns the preserved fd number for the exec
/// argv. The guest copy drops in the caller after spawn, so the
/// framework observes EOF once the wrapper seals at exec.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on wrapper mismatch or a
/// non-crun runtime, and `CistellaError::Runtime` on
/// detection/fcntl failure.
pub fn prepare_diagnostics_hook(diag: &OwnedFd, argv: &mut Vec<String>) -> Result<RawFd> {
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
    let number = raw;
    argv.insert(1, format!("--diagnostics-fd={number}"));
    Ok(number)
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
