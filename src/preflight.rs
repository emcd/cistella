//! Host preflight: cgroup v2, subuid/subgid, podman info, systemd dir.

use std::path::Path;
use std::process::Command;

use crate::error::{CistellaError, Result};

/// Runs the host preflight checks described in `specs/image` and design.
///
/// Checks: command presence, cgroup v2, invoking-user `subuid`/`subgid`,
/// `podman info` `rootless:true` / `cgroupVersion:v2` / `overlay` /
/// `netavark`, `podman unshare`, and `~/.config/containers/systemd/`.
///
/// # Errors
///
/// Returns `CistellaError::Preflight` on any failed check.
pub fn run_preflight() -> Result<()> {
    check_command("podman")?;
    check_command("newuidmap")?;
    check_command("newgidmap")?;
    check_command("slirp4netns")?;
    check_command("fuse-overlayfs")?;
    check_cgroup_v2()?;
    check_subid()?;
    check_podman_info()?;
    check_podman_unshare()?;
    check_systemd_dir()?;
    Ok(())
}

fn check_command(bin: &str) -> Result<()> {
    let out = Command::new("which")
        .arg(bin)
        .output()
        .map_err(|e| CistellaError::Preflight(format!("which {bin} failed: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Preflight(format!("{bin} not found in PATH")));
    }
    Ok(())
}

fn check_cgroup_v2() -> Result<()> {
    let data = std::fs::read_to_string("/proc/filesystems")
        .map_err(|e| CistellaError::Preflight(format!("read /proc/filesystems: {e}")))?;
    if !data.contains("cgroup2") {
        return Err(CistellaError::Preflight(
            "cgroup2 not listed in /proc/filesystems".to_string(),
        ));
    }
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        return Err(CistellaError::Preflight(
            "/sys/fs/cgroup/cgroup.controllers missing (not cgroup v2)".to_string(),
        ));
    }
    Ok(())
}

fn check_subid() -> Result<()> {
    let user = std::env::var("USER").unwrap_or_else(|_| {
        // Fallback to id -un
        String::from_utf8_lossy(
            &Command::new("id")
                .arg("-un")
                .output()
                .map(|o| o.stdout)
                .unwrap_or_default(),
        )
        .trim()
        .to_string()
    });
    if user.is_empty() {
        return Err(CistellaError::Preflight(
            "cannot determine invoking user".to_string(),
        ));
    }
    for file in ["/etc/subuid", "/etc/subgid"] {
        let data = std::fs::read_to_string(file)
            .map_err(|e| CistellaError::Preflight(format!("read {file}: {e}")))?;
        let found = data.lines().any(|l| l.starts_with(&format!("{user}:")));
        if !found {
            return Err(CistellaError::Preflight(format!(
                "{file} missing entry for {user}"
            )));
        }
    }
    Ok(())
}

fn check_podman_info() -> Result<()> {
    let out = Command::new("podman")
        .args(["info", "--format", "json"])
        .output()
        .map_err(|e| CistellaError::Preflight(format!("podman info failed: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Preflight(format!(
            "podman info non-zero: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CistellaError::Preflight(format!("podman info json parse: {e}")))?;
    let rootless = v
        .get("host")
        .and_then(|h| h.get("security"))
        .and_then(|s| s.get("rootless"))
        .and_then(|r| r.as_bool())
        .unwrap_or(false);
    if !rootless {
        return Err(CistellaError::Preflight(
            "podman info host.security.rootless != true".to_string(),
        ));
    }
    let cgroup = v
        .get("host")
        .and_then(|h| h.get("cgroupVersion"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if cgroup != "v2" {
        return Err(CistellaError::Preflight(format!(
            "podman info host.cgroupVersion != v2: {cgroup}"
        )));
    }
    let driver = v
        .get("store")
        .and_then(|s| s.get("graphDriverName"))
        .and_then(|d| d.as_str())
        .unwrap_or("");
    if !driver.to_lowercase().contains("overlay") {
        return Err(CistellaError::Preflight(format!(
            "podman info store.graphDriverName not overlay: {driver}"
        )));
    }
    let backend = v
        .get("host")
        .and_then(|h| h.get("networkBackend"))
        .and_then(|b| b.as_str())
        .unwrap_or("");
    if !backend.to_lowercase().contains("netavark") && !backend.to_lowercase().contains("pasta") {
        return Err(CistellaError::Preflight(format!(
            "podman info host.networkBackend not netavark: {backend}"
        )));
    }
    Ok(())
}

fn check_podman_unshare() -> Result<()> {
    let out = Command::new("podman")
        .args(["unshare", "id", "-u"])
        .output()
        .map_err(|e| CistellaError::Preflight(format!("podman unshare failed: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Preflight(format!(
            "podman unshare non-zero: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

fn check_systemd_dir() -> Result<()> {
    let home =
        std::env::var("HOME").map_err(|_| CistellaError::Preflight("HOME not set".to_string()))?;
    let dir = Path::new(&home).join(".config/containers/systemd");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| CistellaError::Preflight(format!("create {}: {e}", dir.display())))?;
    }
    Ok(())
}
