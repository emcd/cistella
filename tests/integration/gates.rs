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
#[derive(Debug)]
pub enum Probe {
    Available,
    Unavailable(String),
    Broken(String),
}

/// Tri-state systemd user-manager probe: the command RAN and
/// reported absence (`Unavailable`) vs the command itself failed
/// operationally (`Broken`: timeout/overflow/spawn, or signal
/// death — same signal-failure discipline as the podman/image
/// tables; a killed probe is never a missing prerequisite).
pub fn systemd_probe() -> Probe {
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "show-environment"]);
    match run_bounded(cmd, RUN_CISTELLA_TIMEOUT) {
        Ok(out) if out.status.success() => Probe::Available,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            match out.status.code() {
                Some(code) => Probe::Unavailable(format!(
                    "systemd user manager not available (exit {code}): {stderr}"
                )),
                None => Probe::Broken(format!(
                    "systemctl killed by signal (not a missing prerequisite): {stderr}"
                )),
            }
        }
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
        Ok(out) => {
            let combined = format!(
                "stdout: {} stderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            classify_probe_exit("podman info", out.status, &combined, &[])
        }
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

/// Exit-status classification for capability probes: code 0 →
/// Available; listed `missing` codes → Unavailable with stderr
/// retained in the reason; anything else — including signal death
/// (no exit code) — → Broken (operational failure, never a skip).
/// Command-specific tables:
/// - `podman info`: `missing=[]` — any running-but-failing podman
///   on a live-required path is operational except documented
///   absence shapes (none known; the binary-absent case never
///   reaches here — spawn failure is `Broken`).
/// - `podman image exists`: `missing=[1]` (documented absent
///   code); 125 is the daemon/runtime error → `Broken`.
pub fn classify_probe_exit(
    binary: &str,
    status: std::process::ExitStatus,
    evidence: &str,
    missing: &[i32],
) -> Probe {
    if status.success() {
        return Probe::Available;
    }
    match status.code() {
        Some(code) if missing.contains(&code) => Probe::Unavailable(format!(
            "{binary} reported missing capability (exit {code}): {evidence}"
        )),
        Some(code) => Probe::Broken(format!(
            "{binary} probe failed operationally (exit {code}): {evidence}"
        )),
        None => Probe::Broken(format!(
            "{binary} probe killed by signal (not a missing capability): {evidence}"
        )),
    }
}

/// Required image gate for live tests: present proceeds; absent
/// (documented exit 1) soft-skips with stderr retained; 125,
/// signal death, or harness failure PANICS (operational, never a
/// skip).
pub fn image_present(image: &str) -> bool {
    let mut cmd = Command::new("podman");
    cmd.args(["image", "exists", image]);
    match run_bounded(cmd, RUN_CISTELLA_TIMEOUT) {
        Ok(out) => match classify_probe_exit(
            "podman image exists",
            out.status,
            &String::from_utf8_lossy(&out.stderr),
            &[1],
        ) {
            Probe::Available => true,
            Probe::Unavailable(reason) => {
                skip(reason);
                false
            }
            Probe::Broken(detail) => panic!("seat operational failure (not a skip): {detail}"),
        },
        Err(why) => panic!("seat operational failure (not a skip): image probe failed: {why}"),
    }
}

/// Unshare capability probe for bind-mount tests: exit 0 capable;
/// other nonzero (e.g. userns disabled) incapable with stderr
/// retained; 125, signal death, or harness failure PANICS
/// (operational, never a skip).
pub fn podman_unshare_capable() -> bool {
    let mut cmd = Command::new("podman");
    cmd.args(["unshare", "true"]);
    match run_bounded(cmd, RUN_CISTELLA_TIMEOUT) {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            match out.status.code() {
                Some(125) | None => panic!(
                    "seat operational failure (not a skip): unshare probe failed: exit {:?}: {stderr}",
                    out.status.code()
                ),
                Some(code) => {
                    skip(format!(
                        "podman unshare unavailable (exit {code}): {stderr}"
                    ));
                    false
                }
            }
        }
        Err(why) => panic!("seat operational failure (not a skip): unshare probe failed: {why}"),
    }
}
