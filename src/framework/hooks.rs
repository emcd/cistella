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
/// to EOF. Returns the attested ABI plus an exec-failure detail
/// when the wrapper reported one after attesting (exec failed, so
/// the harness never started — the caller reports wrapper failure,
/// never a harness outcome).
///
/// Trailing bytes are counted, never printed: diagnostics past the
/// attestation are wrapper-internal (paths included) and stay out
/// of operator output by contract.
///
/// # Errors
///
/// Returns the attestation reader's `Contract`/`Runtime` failure.
pub fn gate_hook_attestation(
    read: &std::os::fd::OwnedFd,
    timeout: std::time::Duration,
) -> Result<(u64, Option<String>)> {
    use std::os::fd::AsFd;
    let (line, rest) = read_attestation_line(read.as_fd(), timeout)?;
    let abi = parse_attestation_line(&line)?;
    if !rest.is_empty() {
        eprintln!(
            "hook diagnostics: {} trailing bytes after attestation",
            rest.len()
        );
    }
    let exec_failure = drain_hook_diagnostics(read, rest, timeout);
    eprintln!("hook confinement applied (Landlock ABI {abi})");
    Ok((abi, exec_failure))
}

/// Drains hook diagnostics to EOF under the deadline (64 KiB cap),
/// watching for a wrapper exec-failure report. Infallible by
/// design: the session already gated on the attestation, so a slow
/// or chatty wrapper cannot fail a confined harness — and only the
/// wrapper itself can write here (the fd seals at harness exec),
/// so a negative report is always genuine. Returns the first
/// exec-failure detail, if any.
fn drain_hook_diagnostics(
    read: &std::os::fd::OwnedFd,
    buffered: Vec<u8>,
    timeout: std::time::Duration,
) -> Option<String> {
    use crate::framework::prepare::exec_failure_detail;
    use std::os::fd::{AsFd, AsRawFd};
    let deadline = std::time::Instant::now() + timeout;
    let mut pending = buffered;
    let mut total = 0usize;
    loop {
        while let Some(position) = pending.iter().position(|&byte| byte == b'\n') {
            let line = String::from_utf8_lossy(&pending[..position]).into_owned();
            pending.drain(..=position);
            if let Some(detail) = exec_failure_detail(&line) {
                return Some(detail);
            }
        }
        if total > 64 * 1024 {
            return None;
        }
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
            Ok(0) => return None,
            Err(_) => return None,
            Ok(_) => {}
        }
        let mut chunk = [0u8; 8192];
        // SAFETY: borrowed read-end, transient buffer; count
        // checked below, no ownership transfer.
        let count =
            unsafe { nix::libc::read(read.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return None;
        }
        total += count as usize;
        pending.extend_from_slice(&chunk[..count as usize]);
    }
}

/// Hooked-launch plan: wrapper argv plus the diagnostics pipe,
/// composed pre-create (argv) and post-host (pipe, so the guest
/// never inherits it). A single `Option` keeps the launch site
/// infallible: `Some` always carries the complete plan.
pub struct HookLaunchPlan {
    /// Framework-composed wrapper argv (executable, allows,
    /// `--`, harness argv verbatim).
    pub argv: Vec<String>,
    /// Diagnostics read-end (attestation gate + drain).
    pub diag_read: std::os::fd::OwnedFd,
    /// Diagnostics write-end (crosses in the second bundle).
    pub diag_write: std::os::fd::OwnedFd,
}

/// Composes the hooked wrapper argv pre-create (task 3.2): home
/// confinement roots plus `compose_hook_argv`. An untranslatable
/// topology, a session outside the confinement root, or a bad
/// shape refuses here with no unit and no guest.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on missing `HOME` and
/// `CistellaError::Contract` on roots/compose refusal.
pub fn plan_hook_argv(
    hooks: &[crate::framework::contract::GuestHookRequest],
    triples: &[crate::mount::MountTriple],
    directory: &str,
    harness_argv: &[String],
) -> Result<Vec<String>> {
    use crate::framework::prepare::{compose_hook_argv, confinement_roots};
    let home =
        std::env::var("HOME").map_err(|_| CistellaError::Runtime("HOME not set".to_string()))?;
    let (ancestor_host, subtree_host) = confinement_roots(std::path::Path::new(&home), directory)?;
    compose_hook_argv(hooks, triples, &ancestor_host, &subtree_host, harness_argv)
}

/// Creates the diagnostics pipe post-host (a pre-host pipe would
/// leak into the guest's inherited fds and defeat EOF).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on pipe failure.
pub fn plan_hook_launch(argv: Vec<String>) -> Result<HookLaunchPlan> {
    let (diag_read, diag_write) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
        .map_err(|e| CistellaError::Runtime(format!("diagnostics pipe: {e}")))?;
    Ok(HookLaunchPlan {
        argv,
        diag_read,
        diag_write,
    })
}

/// Runs one hooked launch: wrapper argv through the hooked client
/// path, session start gated on the applied attestation. Returns
/// the execution plus a wrapper exec-failure detail when the
/// wrapper attested applied and then failed to exec (the caller
/// reports wrapper failure, never a harness outcome).
///
/// # Errors
///
/// Returns launch, attestation, and teardown-agnostic transport
/// failures; the caller converges residue.
pub fn run_hooked_launch(
    client: &crate::isolators::client::WireClient,
    unit: &crate::framework::contract::UnitHandle,
    plan: HookLaunchPlan,
    workdir: &str,
    key: &crate::framework::contract::ReconciliationKey,
) -> Result<(crate::framework::contract::ExecutionHandle, Option<String>)> {
    use crate::framework::isolator::StdioBinding;
    use std::os::fd::AsFd;
    let result = client.death_checked(client.execute_launch_hooked(
        unit,
        &plan.argv,
        Some(workdir),
        StdioBinding::Inherit,
        key,
        plan.diag_write.as_fd(),
    ));
    // Our write-end copy closes here: the guest holds its own
    // bundle copy, so EOF still tracks the wrapper's seal.
    drop(plan.diag_write);
    match result {
        Ok(execution) => gate_hook_attestation(
            &plan.diag_read,
            crate::framework::contract::Deadlines::default().apply,
        )
        .map(|(_, exec_failure)| (execution, exec_failure)),
        Err(error) => Err(error),
    }
}
