//! Runtime: rootless Podman lifecycle with labels and GC, Quadlet.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CistellaError, Result};

/// Label keys used by the driver.
pub const LABEL_SESSION: &str = "cistella.session-id";
pub const LABEL_SEAT: &str = "cistella.seat";
pub const LABEL_HARNESS: &str = "cistella.harness";
pub const LABEL_PROFILE: &str = "cistella.profile";

/// Session identity for a container.
#[derive(Debug, Clone)]
pub struct SessionId {
    /// Opaque session identifier.
    pub session_id: String,
    /// Seat name.
    pub seat: String,
    /// Harness name.
    pub harness: String,
    /// Profile name.
    pub profile: String,
}

impl SessionId {
    #[must_use]
    pub fn label_args(&self) -> Vec<String> {
        vec![
            "--label".to_string(),
            format!("{}={}", LABEL_SESSION, self.session_id),
            "--label".to_string(),
            format!("{}={}", LABEL_SEAT, self.seat),
            "--label".to_string(),
            format!("{}={}", LABEL_HARNESS, self.harness),
            "--label".to_string(),
            format!("{}={}", LABEL_PROFILE, self.profile),
        ]
    }

    #[must_use]
    pub fn container_name(&self) -> String {
        format!("cistella-{}-{}", self.harness, self.session_id)
    }

    #[must_use]
    pub fn quadlet_unit_name(&self) -> String {
        format!("cistella-{}-{}.container", self.harness, self.session_id)
    }
}

/// Generates a Quadlet `.container` unit for the session.
///
/// The unit uses systemd to own the container, not `podman run --rm`.
/// Removal is `gc`'s job. All interpolated values are validated to prevent
/// Quadlet directive injection (`\n`/`\r`/`\0` rejected).
///
/// Quadlet manpage: `Container` section with `Image`, `ContainerName`,
/// `Label`, `Volume`, `Environment`, `UserNS`.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if any interpolated value contains
/// control characters that would inject Quadlet directives.
pub fn generate_quadlet_unit(
    session: &SessionId,
    image: &str,
    volumes: &[String],
    env_extra: &[String],
    container_home: &str,
) -> Result<String> {
    for (field, val) in [
        ("session_id", &session.session_id),
        ("seat", &session.seat),
        ("harness", &session.harness),
        ("profile", &session.profile),
    ] {
        ensure_session_field(val, field)?;
    }
    ensure_no_injection(image, "image")?;
    ensure_no_injection(container_home, "container_home")?;
    let container_home = crate::mount::canonicalize_container_target(container_home);
    // Env keys are [A-Z_][A-Z0-9_]*, values newline-free.
    for e in env_extra {
        ensure_no_injection(e, "env")?;
        if let Some((k, _)) = e.split_once('=')
            && (k.is_empty()
                || !k
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
                || !k
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        {
            return Err(CistellaError::Runtime(format!(
                "env key must match [A-Z_][A-Z0-9_]*: {k}"
            )));
        }
    }
    for v in volumes {
        ensure_no_injection(v, "volume")?;
        // Volumes must not contain '=' which would inject Quadlet directives; host:container:mode has no '='.
        if v.contains('=') {
            return Err(CistellaError::Runtime(format!(
                "volume must not contain '=': {v}"
            )));
        }
    }
    let mut out = String::new();
    out.push_str("[Unit]\n");
    out.push_str(&format!(
        "Description=Cistella session {} ({})\n",
        session.session_id, session.harness
    ));
    out.push_str("After=network-online.target\n\n");
    out.push_str("[Container]\n");
    out.push_str(&format!("Image={image}\n"));
    out.push_str(&format!("ContainerName={}\n", session.container_name()));
    out.push_str("UserNS=keep-id\n");
    // Labels
    out.push_str(&format!("Label={}={}\n", LABEL_SESSION, session.session_id));
    out.push_str(&format!("Label={}={}\n", LABEL_SEAT, session.seat));
    out.push_str(&format!("Label={}={}\n", LABEL_HARNESS, session.harness));
    out.push_str(&format!("Label={}={}\n", LABEL_PROFILE, session.profile));
    // HOME is static and belongs in the unit; closed TERM env is forwarded at exec time (transport spec), not baked.
    out.push_str(&format!("Environment=HOME={container_home}\n"));
    for e in env_extra {
        out.push_str(&format!("Environment={e}\n"));
    }
    // Volumes: session-home tmpfs + triples + SSH agent
    // Flat list [flag, value, ...] from mount::podman_volume_args: Tmpfs uses dedicated key.
    // Canonicalize container_home for Tmpfs rendering to prevent traversal injection.
    let mut i = 0;
    while i + 1 < volumes.len() {
        let flag = &volumes[i];
        let val = &volumes[i + 1];
        if flag == "--tmpfs" {
            out.push_str(&format!("Tmpfs={container_home}\n"));
        } else if flag == "--volume" {
            out.push_str(&format!("Volume={val}\n"));
        }
        i += 2;
    }
    out.push_str("Exec=sleep infinity\n\n");
    out.push_str("[Service]\nRestart=on-failure\nSuccessExitStatus=143\n\n");
    out.push_str("[Install]\nWantedBy=default.target\n");
    Ok(out)
}

fn ensure_no_injection(val: &str, field: &str) -> Result<()> {
    if val.contains('\n') || val.contains('\r') || val.contains('\0') {
        return Err(CistellaError::Runtime(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

fn ensure_session_field(val: &str, field: &str) -> Result<()> {
    ensure_no_injection(val, field)?;
    if val.is_empty() || val.len() > 64 {
        return Err(CistellaError::Runtime(format!(
            "{field} must be 1-64 chars"
        )));
    }
    if !val
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(CistellaError::Runtime(format!(
            "{field} must match [A-Za-z0-9._-]+: {val}"
        )));
    }
    // Also reject leading dash which could be parsed as flag in systemd Label=
    if val.starts_with('-') {
        return Err(CistellaError::Runtime(format!(
            "{field} must not start with '-': {val}"
        )));
    }
    Ok(())
}

/// Writes a Quadlet unit to `~/.config/containers/systemd/` and reloads.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on IO or systemctl failure when systemd is available.
pub fn install_quadlet(unit_name: &str, content: &str) -> Result<PathBuf> {
    let home =
        std::env::var("HOME").map_err(|_| CistellaError::Runtime("HOME not set".to_string()))?;
    let dir = Path::new(&home).join(".config/containers/systemd");
    std::fs::create_dir_all(&dir)
        .map_err(|e| CistellaError::Runtime(format!("create {}: {e}", dir.display())))?;
    let path = dir.join(unit_name);
    std::fs::write(&path, content)
        .map_err(|e| CistellaError::Runtime(format!("write {}: {e}", path.display())))?;
    if systemd_user_available() {
        let out = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output()
            .map_err(|e| CistellaError::Runtime(format!("systemctl daemon-reload: {e}")))?;
        if !out.status.success() {
            return Err(CistellaError::Runtime(format!(
                "daemon-reload failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
    }
    Ok(path)
}

/// Starts a Quadlet unit.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if systemctl fails.
pub fn start_quadlet(unit_name: &str) -> Result<()> {
    let service = unit_name.replace(".container", ".service");
    let out = Command::new("systemctl")
        .args(["--user", "start", &service])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("systemctl start: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "start {service} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

fn systemd_user_available() -> bool {
    // `systemctl --user show-environment` succeeds only when user systemd is running.
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .is_ok_and(|o| o.status.success())
}
fn query_unit_props(service: &str) -> std::collections::HashMap<String, String> {
    let out = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            service,
        ])
        .output();
    let mut map = std::collections::HashMap::new();
    if let Ok(o) = out
        && o.status.success()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}

/// Stops a session via systemd (Quadlet owner), removing the unit.
///
/// The container is owned by systemd, not `podman run --rm`. Stopping via
/// `podman stop` would be undone by `Restart=on-failure`. This stops the
/// `.service`/`.container` via `systemctl --user`, disables it, removes the
/// unit file and reloads. Fails closed when systemd is present but teardown
/// fails; falls back to `podman stop` only when no user systemd manager exists
/// (e.g. in tests/CI).
///
/// `name` is the container name (`cistella-<harness>-<session_id>`) from
/// `SessionId::container_name`, which maps to `cistella-<harness>-<session_id>.container`.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if systemctl fails.
pub fn stop_container(name: &str) -> Result<()> {
    let unit = format!("{name}.container");
    let service = format!("{name}.service");
    if systemd_user_available() {
        let out = Command::new("systemctl")
            .args(["--user", "stop", &service])
            .output()
            .map_err(|e| CistellaError::Runtime(format!("systemctl stop: {e}")))?;
        if !out.status.success() {
            let props = query_unit_props(&service);
            let not_found = props.get("LoadState").is_some_and(|v| v == "not-found");
            if !not_found {
                return Err(CistellaError::Runtime(format!(
                    "systemctl stop {service} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
        }
        // Wait for service to reach inactive/failed before file removal + reset-failed
        for _ in 0..20 {
            let p = query_unit_props(&service);
            let a = p
                .get("ActiveState")
                .map(|s| s.as_str())
                .unwrap_or("inactive");
            if a == "inactive"
                || a == "failed"
                || p.get("LoadState").is_some_and(|v| v == "not-found")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if let Ok(home) = std::env::var("HOME") {
            let path = Path::new(&home)
                .join(".config/containers/systemd")
                .join(&unit);
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| CistellaError::Runtime(format!("remove {path:?}: {e}")))?;
                let out = Command::new("systemctl")
                    .args(["--user", "daemon-reload"])
                    .output()
                    .map_err(|e| CistellaError::Runtime(format!("systemctl daemon-reload: {e}")))?;
                if !out.status.success() {
                    return Err(CistellaError::Runtime(format!(
                        "daemon-reload failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    )));
                }
            }
        }
        let _ = Command::new("systemctl")
            .args(["--user", "reset-failed", &service])
            .output();
        // Scratch cleanup is done by the caller (main.rs) using the known session_id
        // to avoid lossy parsing of hyphenated session ids from the container name.
        Ok(())
    } else {
        // Fallback for environments without systemd user instance (tests)
        let out = Command::new("podman")
            .args(["stop", "--time", "2", name])
            .output()
            .map_err(|e| CistellaError::Runtime(format!("podman stop: {e}")))?;
        if !out.status.success() {
            return Err(CistellaError::Runtime(format!(
                "stop {name} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(())
    }
}

/// Returns container logs. Under Quadlet systemd ownership `podman logs` is
/// empty because Quadlet adds `--rm`; the real post-mortem is `journalctl --user -u <service>`.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if both journal and podman fail.
pub fn logs_container(name: &str) -> Result<String> {
    let service = format!("{name}.service");
    let jout = Command::new("journalctl")
        .args(["--user", "-u", &service, "--no-pager", "-n", "500"])
        .output();
    if let Ok(o) = jout
        && o.status.success()
        && !o.stdout.is_empty()
    {
        return Ok(String::from_utf8_lossy(&o.stdout).to_string());
    }
    let out = Command::new("podman")
        .args(["logs", name])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman logs: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "logs {name} failed: {} / journal fallback empty",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// GC result.
#[derive(Debug, Clone)]
pub struct GcResult {
    /// Reaped container ids/names.
    pub reaped: Vec<String>,
}

/// Reaps orphaned cistella units and stray non-systemd containers.
/// Under Quadlet `--rm` a stopped/crashed service has no `podman ps` entry,
/// so GC must enumerate BOTH `~/.config/containers/systemd/cistella-*.container`
/// files and `podman ps --all` `cistella.*` labels. For each unit: if
/// `ActiveState` is inactive/failed/not-found the unit file is removed,
/// daemon-reloaded, reset-failed, and scratch cleaned; active units are left.
/// Fail-closed on any inspect error.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if podman list fails; on inspect error
/// returns empty Ok (fail-closed).
pub fn gc_exited() -> Result<GcResult> {
    let mut reaped = Vec::new();
    let systemd = systemd_user_available();
    // Enumerate candidates from both sources: unit files + podman ps
    let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
    // From podman ps --all
    let out = Command::new("podman")
        .args([
            "ps",
            "--all",
            "--filter",
            "label=cistella.session-id",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman ps: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "ps failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    for n in String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        candidates.insert(n);
    }
    // From Quadlet unit directory (orphaned files invisible to ps due to --rm)
    if let Ok(home) = std::env::var("HOME") {
        let dir = Path::new(&home).join(".config/containers/systemd");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let fname = e.file_name().to_string_lossy().to_string();
                if fname.starts_with("cistella-") && fname.ends_with(".container") {
                    let cname = fname.trim_end_matches(".container").to_string();
                    candidates.insert(cname);
                }
            }
        }
    }
    for name in candidates {
        // Determine if this candidate is orphaned: inspect container if exists, otherwise check unit service state.
        let insp = Command::new("podman")
            .args(["inspect", "--format", "{{.State.Status}}", &name])
            .output();
        let (is_exited_container, is_orphan_unit) = match insp {
            Ok(o) if o.status.success() => {
                let status = String::from_utf8_lossy(&o.stdout).trim().to_string();
                (status == "exited", false)
            }
            _ => {
                // No container (Quadlet --rm) — check if unit file exists and service is inactive/failed/not-found
                let mut orphan = false;
                if let Ok(home) = std::env::var("HOME") {
                    let unit = format!("{name}.container");
                    let path = Path::new(&home)
                        .join(".config/containers/systemd")
                        .join(&unit);
                    if path.exists() && systemd {
                        let service = format!("{name}.service");
                        let props = query_unit_props(&service);
                        let active = props
                            .get("ActiveState")
                            .map(|s| s.as_str())
                            .unwrap_or("inactive");
                        let load = props
                            .get("LoadState")
                            .map(|s| s.as_str())
                            .unwrap_or("not-found");
                        if load == "not-found" || active == "inactive" || active == "failed" {
                            orphan = true;
                        }
                    }
                }
                (false, orphan)
            }
        };
        if is_exited_container || is_orphan_unit {
            if systemd {
                let unit = format!("{name}.container");
                let service = format!("{name}.service");
                // Only stop if service is active
                let props = query_unit_props(&service);
                let active = props
                    .get("ActiveState")
                    .map(|s| s.as_str())
                    .unwrap_or("inactive");
                if active == "active" || active == "activating" {
                    let out = Command::new("systemctl")
                        .args(["--user", "stop", &service])
                        .output()
                        .map_err(|e| CistellaError::Runtime(format!("systemctl stop: {e}")))?;
                    if !out.status.success() {
                        let props2 = query_unit_props(&service);
                        let not_found = props2.get("LoadState").is_some_and(|v| v == "not-found");
                        if !not_found {
                            return Err(CistellaError::Runtime(format!(
                                "systemctl stop {service} failed: {}",
                                String::from_utf8_lossy(&out.stderr)
                            )));
                        }
                    }
                    // Wait for inactive
                    for _ in 0..20 {
                        let p = query_unit_props(&service);
                        let a = p
                            .get("ActiveState")
                            .map(|s| s.as_str())
                            .unwrap_or("inactive");
                        if a == "inactive"
                            || a == "failed"
                            || p.get("LoadState").is_some_and(|v| v == "not-found")
                        {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
                // Resolve session id BEFORE removing the unit file (its Label= is the only source for --rm'd orphans)
                let sid = get_session_id_for_gc(&name);
                if let Ok(home) = std::env::var("HOME") {
                    let path = Path::new(&home)
                        .join(".config/containers/systemd")
                        .join(&unit);
                    if path.exists() {
                        std::fs::remove_file(&path)
                            .map_err(|e| CistellaError::Runtime(format!("remove {path:?}: {e}")))?;
                        let out = Command::new("systemctl")
                            .args(["--user", "daemon-reload"])
                            .output()
                            .map_err(|e| {
                                CistellaError::Runtime(format!("systemctl daemon-reload: {e}"))
                            })?;
                        if !out.status.success() {
                            return Err(CistellaError::Runtime(format!(
                                "daemon-reload failed: {}",
                                String::from_utf8_lossy(&out.stderr)
                            )));
                        }
                    }
                }
                let _ = Command::new("systemctl")
                    .args(["--user", "reset-failed", &service])
                    .output();
                if !sid.is_empty() {
                    let _ = std::fs::remove_dir_all(format!("/tmp/cistella-{sid}"));
                }
            }
            // Try podman rm for stray non-systemd containers; for Quadlet orphans there is no container so this is no-op.
            let rm = Command::new("podman").args(["rm", "-f", &name]).output();
            if let Ok(rmo) = rm
                && rmo.status.success()
            {
                reaped.push(name.clone());
            } else if is_orphan_unit || (systemd && is_exited_container) {
                reaped.push(name.clone());
            }
        }
    }
    Ok(GcResult { reaped })
}

fn get_session_id_for_gc(name: &str) -> String {
    // Try podman label first
    if let Ok(o) = Command::new("podman")
        .args([
            "inspect",
            "--format",
            "{{index .Config.Labels \"cistella.session-id\"}}",
            name,
        ])
        .output()
        && o.status.success()
    {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !s.is_empty() && s != "<no value>" {
            return s;
        }
    }
    // Fallback: read Label= from unit file
    if let Ok(home) = std::env::var("HOME") {
        let unit = format!("{name}.container");
        let path = Path::new(&home)
            .join(".config/containers/systemd")
            .join(unit);
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                if let Some(v) = line.strip_prefix("Label=cistella.session-id=") {
                    return v.trim().to_string();
                }
            }
        }
    }
    String::new()
}
