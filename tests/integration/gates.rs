//! Seat-capability probes and gates for the test harness.
//!
//! Split from `helpers.rs` (line-count hook): tri-state probes
//! (Available/Unavailable/Broken) plus the shared soft-skip
//! marker. `helpers.rs` re-exports this module so existing
//! `use super::helpers::*` consumers are unaffected.

use std::process::Command;

use super::bounded::{RUN_CISTELLA_TIMEOUT, run_bounded};

/// Seat-capability probe outcome: three states, never two.
/// `Available` runs; `Unavailable` soft-skips with reason;
/// `Broken` FAILS LOUDLY — an operational failure (timeout,
/// overflow, capture error, spawn failure) is seat evidence,
/// never a missing prerequisite masquerading as a skip.
pub enum Probe {
    Available,
    Unavailable(String),
    Broken(String),
}

/// Tri-state systemd user-manager probe: the command RAN and
/// reported absence (`Unavailable`) vs the command itself failed
/// operationally (`Broken`: timeout/overflow/spawn).
pub fn systemd_probe() -> Probe {
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "show-environment"]);
    match run_bounded(cmd, RUN_CISTELLA_TIMEOUT) {
        Ok(out) if out.status.success() => Probe::Available,
        Ok(out) => Probe::Unavailable(format!(
            "systemd user manager not available (exit {})",
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string())
        )),
        Err(why) => Probe::Broken(format!("systemctl probe failed: {why}")),
    }
}

/// Required live gate: `Available` proceeds; `Unavailable`
/// soft-skips through the shared marker; `Broken` PANICS — a
/// wedged seat must fail loudly, never hide inside the skip
/// roster.
pub fn systemd_gate() -> bool {
    match systemd_probe() {
        Probe::Available => true,
        Probe::Unavailable(reason) => {
            skip(&reason);
            false
        }
        Probe::Broken(detail) => panic!("seat operational failure (not a skip): {detail}"),
    }
}

/// Tri-state podman reachability probe: the command RAN and podman
/// is absent (`Unavailable`) vs the probe itself failed
/// operationally (`Broken`: timeout/overflow/spawn).
pub fn podman_probe() -> Probe {
    let mut cmd = Command::new("podman");
    cmd.args(["info", "--format", "{{.Host.Security.Rootless}}"]);
    match run_bounded(cmd, RUN_CISTELLA_TIMEOUT) {
        Ok(out) if out.status.success() => Probe::Available,
        Ok(out) => Probe::Unavailable(format!(
            "podman not available (exit {})",
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string())
        )),
        Err(why) => Probe::Broken(format!("podman probe failed: {why}")),
    }
}

/// Required podman gate: same tri-state contract as
/// `systemd_gate`.
pub fn podman_gate() -> bool {
    match podman_probe() {
        Probe::Available => true,
        Probe::Unavailable(reason) => {
            skip(&reason);
            false
        }
        Probe::Broken(detail) => panic!("seat operational failure (not a skip): {detail}"),
    }
}

/// Uniform soft-skip log for environment gates: every seat-
/// capability skip speaks this marker (`skip: ...`) so skip rosters
/// grep cleanly from captured output. A skip records APPLICABILITY,
/// never execution — the body does not run, and this line is its
/// only trace. Capability tags (the `#[ignore]` reason) live beside
/// the gate check that enforces them; this marker is the shared
/// spelling that keeps them from rotting apart.
pub fn skip(reason: impl std::fmt::Display) {
    eprintln!("skip: {reason}");
}
