//! Hook conduct mechanics (task 3.2): guest-context probe and
//! attestation gating for wrapper-confined sessions. Pure gates
//! (probe/attestation parse, roots, argv composition) live in
//! [`super::prepare`]; this module owns the podman/diagnostics I/O
//! shells around them.

use crate::error::{CistellaError, Result};
use crate::framework::prepare::{
    parse_attestation_line, parse_probe_report, read_attestation_line,
};

/// Probes the staged wrapper inside the running container (task
/// 3.2): `podman exec` runs `--probe`, and the ABI-plus-mask gate
/// admits only kernels that enforce the full matrix. Bounded wait
/// with kill on timeout; shortfall fails pre-execute typed.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on spawn/wait failure and
/// `CistellaError::Contract` on timeout, probe failure, or matrix
/// shortfall.
pub fn probe_landlock_wrapper(container: &str, timeout: std::time::Duration) -> Result<()> {
    use crate::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use CistellaError;
    let mut child = std::process::Command::new("podman")
        .args(["exec", container, STAGED_WRAPPER_GUEST_PATH, "--probe"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| CistellaError::Runtime(format!("probe spawn: {e}")))?;
    let deadline = std::time::Instant::now() + timeout;
    let output = loop {
        match child
            .try_wait()
            .map_err(|e| CistellaError::Runtime(format!("probe wait: {e}")))?
        {
            Some(_) => {
                break child
                    .wait_with_output()
                    .map_err(|e| CistellaError::Runtime(format!("probe wait: {e}")))?;
            }
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CistellaError::Contract("probe timed out".to_string()));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };
    if !output.status.success() {
        // The wrapper prints {"unsupported":...} on its way out;
        // surface that vocabulary when present, else the raw exit.
        if let Err(error) = parse_probe_report(&output.stdout) {
            let message = error.to_string();
            if message.contains("landlock unsupported") {
                return Err(error);
            }
        }
        return Err(CistellaError::Runtime(format!(
            "probe failed: exit {}",
            output.status
        )));
    }
    parse_probe_report(&output.stdout)
}

/// Gates session start on the wrapper's applied attestation (task
/// 3.2): reads one line under the deadline, refuses negative or
/// malformed attestations typed, then drains remaining diagnostics
/// for operator visibility.
///
/// # Errors
///
/// Returns the attestation reader's `Contract`/`Runtime` failure.
pub fn gate_hook_attestation(
    read: &std::os::fd::OwnedFd,
    timeout: std::time::Duration,
) -> Result<()> {
    use std::os::fd::AsFd;
    let (line, rest) = read_attestation_line(read.as_fd(), timeout)?;
    let abi = parse_attestation_line(&line)?;
    if !rest.is_empty() {
        eprintln!("hook diagnostics: {}", String::from_utf8_lossy(&rest));
    }
    drain_hook_diagnostics(read, timeout);
    eprintln!("hook confinement applied (Landlock ABI {abi})");
    Ok(())
}

/// Drains hook diagnostics to EOF under the deadline (64 KiB cap),
/// printing for operator visibility. Infallible by design: the
/// session already gated on the attestation, so a slow or chatty
/// wrapper cannot fail a confined harness — truncation notes
/// itself on stderr.
fn drain_hook_diagnostics(read: &std::os::fd::OwnedFd, timeout: std::time::Duration) {
    use std::os::fd::{AsFd, AsRawFd};
    let deadline = std::time::Instant::now() + timeout;
    let mut total = 0usize;
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or(std::time::Duration::ZERO);
        let mut pollfds = [nix::poll::PollFd::new(
            read.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        let wait =
            nix::poll::PollTimeout::try_from(remaining).unwrap_or(nix::poll::PollTimeout::ZERO);
        match nix::poll::poll(&mut pollfds, wait) {
            Ok(0) => {
                eprintln!("hook diagnostics: drain timed out");
                return;
            }
            Err(_) => return,
            Ok(_) => {}
        }
        let mut chunk = [0u8; 8192];
        // SAFETY: borrowed read-end, transient buffer; count
        // checked below, no ownership transfer.
        let count =
            unsafe { nix::libc::read(read.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return;
        }
        total += count as usize;
        eprintln!(
            "hook diagnostics: {}",
            String::from_utf8_lossy(&chunk[..count as usize])
        );
        if total > 64 * 1024 {
            eprintln!("hook diagnostics: truncated at 64 KiB");
            return;
        }
    }
}
