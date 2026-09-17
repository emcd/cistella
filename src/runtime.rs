//! Runtime: Quadlet lifecycle, shared teardown, and orphan reaping.
//!
//! Systemd owns each session container via a generated `.container` unit;
//! removal is `teardown`'s job, shared by `conduct`, `terminate`, and `gc`.
//! Session identity lives in `session`; the registry lives in `registry`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use crate::error::{CistellaError, Result};
use crate::lock::{LockGuard, legacy_scratch_dir, scratch_dir};
use crate::registry::unit_file_label;
use crate::session::{
    LABEL_ID, Session, command_label, ensure_minted_id, ensure_no_injection, ensure_session_field,
    validate_generic_label,
};

/// Quotes a value for a Quadlet `Label=` or `Environment=` directive.
///
/// Wraps in double quotes and escapes `%` as `%%` plus `"`/`\` with a
/// backslash. Quadlet unquotes these directives when parsing the unit and
/// re-quotes for the generated `ExecStart`, and systemd expands `%`
/// specifiers (`%h`, `%o`, ...) at start time — so a bare `%h` in argv
/// arrives as `/home/me` unless doubled (verified live: unit
/// `...%h...` yields podman-side `/home/me`).
///
/// `Image=`, `Volume=`, and `Tmpfs=` must stay RAW (see `escape_percent`):
/// Quadlet escapes those for `ExecStart` itself (verified against
/// generated units), so quoting them double-escapes — a quoted `Image=`
/// fails with `invalid reference format` and a quoted `Volume=` keeps
/// literal quotes in the mount.
#[must_use]
pub fn quote_systemd(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '%' => out.push_str("%%"),
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escapes `%` as `%%` for RAW directives (`Image=`, `Volume=`, `Tmpfs=`).
///
/// These values pass through Quadlet to `ExecStart` unquoted, where
/// systemd would otherwise expand `%` specifiers at start time (a host
/// path like `/tmp/100%hoff` would mount the expanded path). Doubling is
/// honored there (verified live: `Volume=/tmp/pct100%%:...` mounts
/// `/tmp/pct100%`), while wrapping or backslash-escaping breaks them.
#[must_use]
pub fn escape_percent(value: &str) -> String {
    value.replace('%', "%%")
}

/// Reverses `quote_systemd` when reading unit files back.
///
/// Parses one `"..."` string with `%%`/`\"`/`\\` escapes and stops at the
/// closing quote; anything else is returned as-is (tolerating unquoted
/// values).
#[must_use]
pub fn unquote_systemd(value: &str) -> String {
    if !value.starts_with('"') {
        return value.to_string();
    }
    let mut out = String::new();
    let mut chars = value[1..].chars();
    loop {
        match chars.next() {
            None => break,
            Some('%') => match chars.next() {
                // `%%` is a literal percent; a bare `%` followed by anything
                // else only occurs in unquoted legacy values, keep both chars.
                Some('%') => out.push('%'),
                Some(c) => {
                    out.push('%');
                    out.push(c);
                }
                None => out.push('%'),
            },
            Some('\\') => match chars.next() {
                None => out.push('\\'),
                Some(c) => out.push(c),
            },
            Some('"') => break,
            Some(c) => out.push(c),
        }
    }
    out
}

/// Resolves an image tag or digest to the digest recorded in `cistella.image`.
///
/// A reference already containing `@` is used as-is when present locally;
/// a tag is resolved via `podman image inspect`. Nothing is pulled: a tag
/// that does not resolve locally is a typed refusal naming the image.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the image is not present locally.
pub fn resolve_image_digest(image: &str) -> Result<String> {
    ensure_no_injection(image, "image")?;
    let exists = Command::new("podman")
        .args(["image", "exists", image])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman image exists: {e}")))?;
    if !exists.status.success() {
        return Err(CistellaError::Runtime(format!(
            "image {image} not present locally: pull it, or run check"
        )));
    }
    if image.contains('@') {
        return Ok(image.to_string());
    }
    let out = Command::new("podman")
        .args(["image", "inspect", "--format", "{{.Digest}}", image])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman image inspect: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "image {image} not present locally: pull it, or run check"
        )));
    }
    let digest = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if digest.is_empty() || digest.contains('\n') {
        return Err(CistellaError::Runtime(format!(
            "image {image} has no digest: pull it, or run check"
        )));
    }
    Ok(format!("{image}@{digest}"))
}

/// Generates a Quadlet `.container` unit for the session.
///
/// The unit uses systemd to own the container, not `podman run --rm`.
/// Removal is `teardown`'s job. All interpolated values are validated to
/// prevent Quadlet directive injection (`\n`/`\r`/`\0` rejected).
///
/// Quadlet manpage: `Container` section with `Image`, `ContainerName`,
/// `Label`, `Volume`, `Environment`, `UserNS`, `RunInit`.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if any interpolated value is invalid.
pub fn generate_quadlet_unit(
    session: &Session,
    volumes: &[String],
    env_extra: &[String],
    generic_labels: &[(String, String)],
) -> Result<String> {
    use crate::session::{
        LABEL_COMMAND, LABEL_DIRECTORY, LABEL_ID, LABEL_IDENTITY, LABEL_IMAGE, LABEL_PROFILE,
        LABEL_PROFILE_DIGEST,
    };
    ensure_minted_id(&session.id)?;
    ensure_session_field(&session.profile, "profile")?;
    ensure_session_field(&session.identity, "identity")?;
    ensure_no_injection(&session.directory, "directory")?;
    if !session.directory.starts_with('/') {
        return Err(CistellaError::Runtime(
            "directory must be absolute".to_string(),
        ));
    }
    ensure_no_injection(&session.profile_digest, "profile-digest")?;
    ensure_no_injection(&session.image, "image")?;
    let command = command_label(&session.command);
    ensure_no_injection(&command, "command")?;
    for (key, value) in generic_labels {
        validate_generic_label(key, value).map_err(CistellaError::Runtime)?;
    }
    let container_home = crate::mount::canonicalize_container_target(&session.container_home);
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
    out.push_str(&format!("Description=Cistella session {}\n", session.id));
    out.push_str("After=network-online.target\n\n");
    out.push_str("[Container]\n");
    // Image stays raw (but %-escaped): Quadlet escapes it for ExecStart
    // itself and a quoted Image fails with `invalid reference format`
    // (verified live).
    out.push_str(&format!("Image={}\n", escape_percent(&session.image)));
    out.push_str(&format!("ContainerName={}\n", session.container_name()));
    out.push_str("UserNS=keep-id\n");
    // tini as PID 1 via the declarative key: forwards SIGTERM to
    // `sleep infinity` (bare PID 1 ignores it by default disposition,
    // stalling stop for the full StopTimeout before the SIGKILL fallback)
    // and reaps zombies. Validated against this fleet's Quadlet
    // (podman 4.9.3): the generator accepts `RunInit`, PID 1 arrives as
    // `podman-init`, stop completes in ~0.25 s.
    out.push_str("RunInit=true\n");
    // Generic labels first, driver-owned `cistella.*` last as defense in
    // depth: only the driver emits the reserved prefix.
    for (key, value) in generic_labels {
        out.push_str(&format!("Label={key}={}\n", quote_systemd(value)));
    }
    out.push_str(&format!(
        "Label={}={}\n",
        LABEL_ID,
        quote_systemd(&session.id)
    ));
    out.push_str(&format!(
        "Label={}={}\n",
        LABEL_DIRECTORY,
        quote_systemd(&session.directory)
    ));
    out.push_str(&format!(
        "Label={}={}\n",
        LABEL_PROFILE,
        quote_systemd(&session.profile)
    ));
    out.push_str(&format!(
        "Label={}={}\n",
        LABEL_PROFILE_DIGEST,
        quote_systemd(&session.profile_digest)
    ));
    out.push_str(&format!(
        "Label={}={}\n",
        LABEL_IDENTITY,
        quote_systemd(&session.identity)
    ));
    out.push_str(&format!(
        "Label={}={}\n",
        LABEL_COMMAND,
        quote_systemd(&command)
    ));
    out.push_str(&format!(
        "Label={}={}\n",
        LABEL_IMAGE,
        quote_systemd(&session.image)
    ));
    // HOME is static and belongs in the unit; closed TERM env is forwarded at exec time (transport spec), not baked.
    out.push_str(&format!(
        "Environment=HOME={}\n",
        quote_systemd(&container_home)
    ));
    for e in env_extra {
        if let Some((k, v)) = e.split_once('=') {
            out.push_str(&format!("Environment={k}={}\n", quote_systemd(v)));
        } else {
            out.push_str(&format!("Environment={e}\n"));
        }
    }
    // Volumes: session-home tmpfs + triples + SSH agent
    // Flat list [flag, value, ...] from mount::podman_volume_args: Tmpfs uses dedicated key.
    // Canonicalize container_home for Tmpfs rendering to prevent traversal injection.
    let mut i = 0;
    while i + 1 < volumes.len() {
        let flag = &volumes[i];
        let val = &volumes[i + 1];
        // Tmpfs/Volume stay raw (but %-escaped): Quadlet quotes them for
        // ExecStart itself (a raw `Volume=/tmp/my dir:/work:rw` arrives
        // intact, verified live); quoting here would keep literal quotes
        // in the mount, while a bare `%` would expand at start time.
        if flag == "--tmpfs" {
            out.push_str(&format!("Tmpfs={}\n", escape_percent(&container_home)));
        } else if flag == "--volume" {
            out.push_str(&format!("Volume={}\n", escape_percent(val)));
        }
        i += 2;
    }
    out.push_str("Exec=sleep infinity\n\n");
    out.push_str("[Service]\nRestart=on-failure\nSuccessExitStatus=143\n\n");
    out.push_str("[Install]\nWantedBy=default.target\n");
    Ok(out)
}

/// Returns the Quadlet unit directory (`~/.config/containers/systemd/`).
#[must_use]
pub fn quadlet_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| std::path::Path::new(&home).join(".config/containers/systemd"))
}

/// Writes a Quadlet unit to `~/.config/containers/systemd/` and reloads.
///
/// Callers must hold the creation-window lock before any unit-file or
/// scratch creation; this function only writes.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on IO or systemctl failure when systemd is available.
pub fn install_quadlet(unit_name: &str, content: &str) -> Result<PathBuf> {
    let Some(dir) = quadlet_dir() else {
        return Err(CistellaError::Runtime("HOME not set".to_string()));
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| CistellaError::Runtime(format!("create {}: {e}", dir.display())))?;
    let path = dir.join(unit_name);
    std::fs::write(&path, content)
        .map_err(|e| CistellaError::Runtime(format!("write {}: {e}", path.display())))?;
    if systemd_user_available() {
        let out = systemctl_user()
            .args(["daemon-reload"])
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

/// Builds `systemctl --user` with a pinned D-Bus timeout.
///
/// sd-bus method calls time out at 25 s by default; pinning
/// `SYSTEMD_BUS_TIMEOUT` explicitly documents the bound so no
/// driver-spawned call holds the creation-window lock past it.
fn systemctl_user() -> Command {
    let mut command = Command::new("systemctl");
    command.arg("--user").env("SYSTEMD_BUS_TIMEOUT", "25s");
    command
}

/// Starts a Quadlet unit.
///
/// Uses `--no-block` and polls `ActiveState` with a 60 s deadline, so a
/// hung start cannot hold the creation-window lock (freezing
/// `gc`/`terminate` fleet-wide) forever. The invocation itself is bounded
/// by the pinned bus timeout (see `systemctl_user`). On any bound breach
/// the caller tears down.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if systemctl fails, the invocation
/// hangs, the service enters `failed`, or the deadline expires.
pub fn start_quadlet(unit_name: &str) -> Result<()> {
    use std::time::{Duration, Instant};
    let service = unit_name.replace(".container", ".service");
    let out = systemctl_user()
        .args(["start", "--no-block", &service])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("systemctl start: {e}")))?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "start {service} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let props = query_unit_props(&service);
        match props.get("ActiveState").map(|s| s.as_str()) {
            Some("active") => return Ok(()),
            Some("failed") => {
                return Err(CistellaError::Runtime(format!(
                    "start {service} entered failed"
                )));
            }
            _ => {}
        }
        if Instant::now() > deadline {
            return Err(CistellaError::Runtime(format!(
                "start {service} timed out after 60s"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Reports whether a systemd user manager is running.
#[must_use]
pub fn systemd_user_available() -> bool {
    // `systemctl --user show-environment` succeeds only when user systemd is running.
    systemctl_user()
        .args(["show-environment"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Queries `LoadState`/`ActiveState` for a user service.
pub(crate) fn query_unit_props(service: &str) -> HashMap<String, String> {
    let out = systemctl_user()
        .args(["show", "-p", "LoadState", "-p", "ActiveState", service])
        .output();
    let mut map = HashMap::new();
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

/// Removes both scratch paths for a session id (XDG plus legacy `/tmp`).
///
/// A missing path is not an error; any other removal failure propagates so
/// a failed cleanup cannot report success with residue left behind.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when a present scratch path cannot be
/// removed.
pub fn remove_scratch(session_id: &str) -> Result<()> {
    for path in [scratch_dir(session_id), legacy_scratch_dir(session_id)] {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(CistellaError::Runtime(format!(
                    "remove scratch {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Shared teardown: stops the unit, waits for it to settle, removes the
/// unit file, reloads, resets failure state, and removes scratch.
///
/// Acquires the creation-window lock; callers that already hold it (like
/// `gc`) use `teardown_inner` instead (nested `flock` on the same path in
/// one process deadlocks).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on systemctl or IO failure (fails
/// closed unless the unit is already `not-found`).
pub fn teardown(container_name: &str, session_id: &str) -> Result<()> {
    let _guard = LockGuard::acquire()?;
    teardown_inner(container_name, session_id)
}

/// Lock-held half of `teardown` for callers holding the creation-window
/// lock across scan-and-teardown.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on systemctl or IO failure.
pub fn teardown_inner(container_name: &str, session_id: &str) -> Result<()> {
    let service = format!("{container_name}.service");
    let unit = format!("{container_name}.container");
    if systemd_user_available() {
        let out = systemctl_user()
            .args(["stop", &service])
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
        if let Some(dir) = quadlet_dir() {
            let path = dir.join(&unit);
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| CistellaError::Runtime(format!("remove {path:?}: {e}")))?;
                let out = systemctl_user()
                    .args(["daemon-reload"])
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
        let _ = systemctl_user().args(["reset-failed", &service]).output();
    } else {
        // Fallback for environments without systemd user instance (tests)
        let out = Command::new("podman")
            .args(["stop", "--time", "2", container_name])
            .output()
            .map_err(|e| CistellaError::Runtime(format!("podman stop: {e}")))?;
        if !out.status.success() {
            return Err(CistellaError::Runtime(format!(
                "stop {container_name} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
    }
    if !session_id.is_empty() {
        // New XDG scratch plus legacy /tmp path; both must go, and a
        // removal failure fails teardown rather than reporting success.
        remove_scratch(session_id)?;
    }
    Ok(())
}

/// Reports whether teardown residue is gone: the unit file is absent and
/// both scratch paths are removed.
///
/// Lets `conduct` tell a benign race (another `terminate` reaped
/// everything concurrently) from a real teardown failure that must fail
/// the invocation even when the harness itself succeeded.
#[must_use]
pub fn residue_gone(container_name: &str, session_id: &str) -> bool {
    residue_gone_for_paths(
        &quadlet_dir()
            .map(|dir| dir.join(format!("{container_name}.container")))
            .unwrap_or_default(),
        &[scratch_dir(session_id), legacy_scratch_dir(session_id)],
    )
}

/// Pure half of `residue_gone` over explicit paths (unit-testable).
#[must_use]
pub fn residue_gone_for_paths(unit_file: &std::path::Path, scratches: &[PathBuf]) -> bool {
    let unit_gone = unit_file.as_os_str().is_empty() || !unit_file.exists();
    unit_gone && scratches.iter().all(|p| !p.exists())
}

/// Returns container logs. Under Quadlet systemd ownership `podman logs` is
/// empty because Quadlet adds `--rm`; the post-mortem is
/// `journalctl --user -u <service>`.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the journal is unavailable under
/// systemd, or when both journal and podman fail without systemd.
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
    if systemd_user_available() {
        return Err(CistellaError::Runtime(format!(
            "journal unavailable for {service}: journalctl --user -u {service}"
        )));
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
    /// Reaped container names.
    pub reaped: Vec<String>,
}

/// Reaps orphaned cistella units and stray non-systemd containers.
///
/// Under Quadlet `--rm` a stopped/crashed service has no `podman ps` entry,
/// so GC enumerates BOTH `~/.config/containers/systemd/cistella-*.container`
/// files and `podman ps --all` `cistella.id` labels. For each candidate: if
/// `ActiveState` is inactive/failed/not-found the shared teardown runs;
/// active units are left. Holds the creation-window lock around the whole
/// scan-and-teardown so a concurrent `conduct` install is never reaped.
/// Fail-closed on any inspect error.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if podman list fails; on inspect error
/// returns empty Ok (fail-closed).
pub fn gc_exited() -> Result<GcResult> {
    let _guard = LockGuard::acquire()?;
    gc_exited_locked()
}

/// Lock-held half of `gc_exited` for the creation-window test, which holds
/// the guard to prove a concurrent `gc` reaps nothing.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if podman list fails.
pub fn gc_exited_locked() -> Result<GcResult> {
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
            "label=cistella.id",
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
    if let Some(dir) = quadlet_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for e in entries.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            if fname.starts_with("cistella-") && fname.ends_with(".container") {
                let cname = fname.trim_end_matches(".container").to_string();
                candidates.insert(cname);
            }
        }
    }
    // Phase 1: classify every candidate before mutating anything. Any
    // inspect/exists failure aborts here with zero teardowns, so "fail
    // closed on any inspect error" holds regardless of HashSet order.
    // Each entry is (container name, session id, reap-as-orphan-unit). The
    // session id comes from the SAME inspect call as the status (plus the
    // unit file for absent containers): no second inspect whose failure
    // could be silently swallowed.
    let mut to_reap: Vec<(String, String, bool)> = Vec::new();
    let mut ordered: Vec<String> = candidates.into_iter().collect();
    ordered.sort();
    for name in ordered {
        // Absence primitive is `podman container exists`: exit 0 present,
        // exit 1 absent, anything else a typed error that reaps nothing.
        // A failed `inspect` alone must never read as absence (fail closed).
        let insp = Command::new("podman")
            .args([
                "inspect",
                "--format",
                "{{.State.Status}} {{index .Config.Labels \"cistella.id\"}}",
                &name,
            ])
            .output();
        let (inspected, inspect_err) = match insp {
            Ok(o) if o.status.success() => (
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
                String::new(),
            ),
            Ok(o) => (None, String::from_utf8_lossy(&o.stderr).to_string()),
            Err(e) => (None, e.to_string()),
        };
        // Status and id ride one call: `<status> <id>`, with `<no value>`
        // (whitespace-split) when the container carries no `cistella.id`
        // label. Minted ids never contain whitespace, so parts[1] alone is
        // exact; anything starting with `<` is podman's missing marker.
        let (status, mut sid) = match inspected.as_deref() {
            Some(out) => {
                let parts: Vec<&str> = out.split_whitespace().collect();
                let status = parts.first().unwrap_or(&"").to_string();
                let sid = match parts.get(1) {
                    Some(id) if !id.starts_with('<') => id.to_string(),
                    _ => String::new(),
                };
                (Some(status), sid)
            }
            None => (None, String::new()),
        };
        let (is_exited_container, is_orphan_unit) = match status.as_deref() {
            Some(status) => (status == "exited", false),
            None => {
                let exists = Command::new("podman")
                    .args(["container", "exists", &name])
                    .output()
                    .map_err(|e| CistellaError::Runtime(format!("podman container exists: {e}")))?;
                if exists.status.success() {
                    // Present but uninspectable: a real invocation failure,
                    // not Quadlet `--rm` absence. Fail closed.
                    return Err(CistellaError::Runtime(format!(
                        "podman inspect {name} failed while container exists: {inspect_err}"
                    )));
                }
                if exists.status.code() != Some(1) {
                    return Err(CistellaError::Runtime(format!(
                        "podman container exists {name}: {}",
                        String::from_utf8_lossy(&exists.stderr)
                    )));
                }
                // Expected absence under Quadlet --rm — check if unit file exists and service is inactive/failed/not-found.
                // The session id comes from the unit file (pure fs read, no
                // podman): the only source for `--rm`'d orphans.
                let mut orphan = false;
                if let Some(dir) = quadlet_dir() {
                    let path = dir.join(format!("{name}.container"));
                    if path.exists() {
                        if sid.is_empty() {
                            sid = unit_file_label(&path, LABEL_ID).unwrap_or_default();
                        }
                        if systemd {
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
                }
                (false, orphan)
            }
        };
        if is_exited_container || is_orphan_unit {
            to_reap.push((name, sid, is_orphan_unit));
        }
    }
    // Phase 2: teardown only classified orphans. A teardown failure still
    // propagates, but every candidate was classified before the first
    // mutation, so no inspect error can follow a deletion.
    for (name, sid, is_orphan_unit) in to_reap {
        if systemd {
            teardown_inner(&name, &sid)?;
        }
        // Try podman rm for stray non-systemd containers; for Quadlet orphans there is no container so this is no-op.
        let rm = Command::new("podman").args(["rm", "-f", &name]).output();
        if let Ok(rmo) = rm
            && rmo.status.success()
        {
            reaped.push(name.clone());
        } else if is_orphan_unit {
            reaped.push(name.clone());
        }
    }
    Ok(GcResult { reaped })
}
