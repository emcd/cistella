//! Mountpoint preparation: seat-owned parents for bind targets.
//! Pure candidate computation plus bounded in-container setup execs.

use std::process::Command;

use crate::error::{CistellaError, Result};

/// Deadline override for tests (`CISTELLA_PREPARE_EXEC_TIMEOUT_MS`); every
/// preparation exec carries the resulting bound, so a wedged container
/// runtime fails the step instead of hanging the creation window.
fn prepare_timeout() -> std::time::Duration {
    let ms = std::env::var("CISTELLA_PREPARE_EXEC_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);
    std::time::Duration::from_millis(ms)
}

/// True when `path` lies strictly beneath `ancestor` (`/a/b` under `/a`;
/// `/a` itself and `/a-b` do not).
fn is_beneath(path: &str, ancestor: &str) -> bool {
    path.len() > ancestor.len()
        && path.starts_with(ancestor)
        && path.as_bytes().get(ancestor.len()) == Some(&b'/')
}

/// Lexical ancestor candidates for mountpoint preparation (non-authoritative).
///
/// Unions every target's ancestor chain, stopping at the tmpfs home or
/// `/` (whichever comes first): the walk never ascends above the home
/// for under-home targets, so image directories outside home are not
/// even candidates. Drops paths equal to any bind target, the tmpfs home
/// itself, or `/`, and paths beneath any bind target — except beneath
/// the tmpfs home, whose subtrees stay eligible as container-ephemeral.
/// Sorted and de-duplicated for stable exec argv. Authority is decided
/// per candidate at runtime (see `preparation_authorized`); this set
/// only focuses the execs.
#[must_use]
pub fn preparation_candidates(targets: &[String], container_home: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for target in targets {
        if !target.starts_with('/') {
            continue;
        }
        let mut rest = target.as_str();
        while let Some((parent, _)) = rest.rsplit_once('/') {
            if parent.is_empty() || parent == container_home {
                break;
            }
            let dominated = targets.iter().any(|t| parent == t || is_beneath(parent, t));
            if !dominated {
                out.push(parent.to_string());
            }
            rest = parent;
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Authorization predicate for one fully-resolved candidate.
///
/// Denies `/`, anything equal to a bind target, and anything beneath a
/// bind target — checked before (and therefore overriding) tmpfs-home
/// eligibility, so a home-lexical path resolving into a bind mount is
/// refused. Everything else (tmpfs-home subtrees, container-local
/// overlay paths) is eligible.
#[must_use]
pub fn preparation_authorized(resolved: &str, bind_targets: &[String]) -> bool {
    if resolved == "/" {
        return false;
    }
    for target in bind_targets {
        if resolved == target || is_beneath(resolved, target) {
            return false;
        }
    }
    true
}

/// Runs a host command with an explicit wall-clock deadline, SIGKILLing on
/// breach and reaping the child so no zombie escapes.
fn run_deadline(
    program: &str,
    args: &[String],
    timeout: std::time::Duration,
) -> Result<std::process::Output> {
    use std::process::Stdio;
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CistellaError::Runtime(format!("spawn {program}: {e}")))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|s| {
        s.spawn(|| {
            let _ = tx.send(child.wait_with_output());
        });
        match rx.recv_timeout(timeout) {
            Ok(out) => out.map_err(|e| CistellaError::Runtime(format!("{program} io: {e}"))),
            Err(_) => {
                // Kills the host client; the in-container exec cannot
                // outlive the failure because every preparation error is
                // followed by full teardown before conduct reports.
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                let _ = rx.recv();
                Err(CistellaError::Runtime(format!(
                    "{program} exceeded {}ms deadline",
                    timeout.as_millis()
                )))
            }
        }
    })
}

/// Builds the `podman exec` argv: `user` pins `--user` (setup mutations
/// run as root; probes run as the default seat user so they observe
/// exactly what the seat observes). Paths always follow `--`, never
/// through a shell.
fn podman_exec_argv(
    container: &str,
    user: Option<&str>,
    cmd: &str,
    args: &[String],
) -> Vec<String> {
    let mut argv = vec!["exec".to_string()];
    if let Some(u) = user {
        argv.push("--user".to_string());
        argv.push(u.to_string());
    }
    argv.push(container.to_string());
    argv.push(cmd.to_string());
    argv.extend(args.iter().cloned());
    argv
}

/// Runs `podman exec`, failing closed on deadline breach or nonzero exit.
fn podman_exec(
    container: &str,
    user: Option<&str>,
    cmd: &str,
    args: &[String],
    timeout: std::time::Duration,
) -> Result<std::process::Output> {
    let argv = podman_exec_argv(container, user, cmd, args);
    let out = run_deadline("podman", &argv, timeout)?;
    if !out.status.success() {
        return Err(CistellaError::Runtime(format!(
            "podman exec {cmd} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out)
}

/// Parses `stat -c '%u %n'` output into name→uid: uid first so names with
/// spaces survive (newlines cannot appear — control characters are
/// rejected at validation). Missing paths simply have no entry, which is
/// exactly the created-vs-preexisting signal the chown rule needs.
#[must_use]
pub fn parse_stat_map(output: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in output.lines() {
        if let Some((uid, name)) = line.split_once(' ') {
            map.insert(name.to_string(), uid.to_string());
        }
    }
    map
}

/// Prepares mountpoint parents after start, before harness attach.
///
/// `sources` seeds lexical candidates (user mounts only);
/// `binds` is the complete emitted bind set authorizing them.
/// Resolves both in-container in one batch (`realpath -m`) and
/// authorizes resolved-against-resolved, so image-baked symlink
/// components in bind targets cannot hide a host-backed path behind a
/// lexical mismatch — then `mkdir -p`s the authorized set and chowns
/// the root-owned subset to the live seat uid.
/// Chown is conditional on root ownership rather than
/// createdness: podman pre-creates missing parents before preparation
/// runs, so "absent before mkdir" would spare exactly the dirs this step
/// exists to fix — while pre-existing image directories keep whatever
/// ownership they have unless root-owned, in which case converging them
/// seat-owned is safe (container-local upper, per-session ephemeral;
/// bind mounts and `/` are structurally excluded, never chowned).
/// Each step is a bounded `podman exec` (worst case five: uid, resolve,
/// mkdir, stat, conditional chown). Operates on resolved paths so static
/// symlinks cannot divert a mutation into a bind mount; dynamic symlink-swap
/// scope under the recorded trusted-image assumption (no untrusted actor
/// exists between these execs and harness attach — and the harness itself
/// would follow swapped links anyway). Callers hold the creation-window
/// lock and fail closed via `teardown_inner`; teardown completion before
/// conduct reports is what bounds any in-container exec past a host-side
/// deadline breach.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on exec failure, deadline breach,
/// unparseable uid, resolve/count mismatch, or refused containment.
pub fn prepare_mountpoints(
    container: &str,
    sources: &[String],
    binds: &[String],
    container_home: &str,
) -> Result<()> {
    let timeout = prepare_timeout();
    let candidates = preparation_candidates(sources, container_home);
    if candidates.is_empty() {
        return Ok(());
    }
    // Seat uid: one bounded integer, no default on any other shape.
    let uid_out = podman_exec(container, None, "id", &["-u".to_string()], timeout)?;
    let uid_text = String::from_utf8_lossy(&uid_out.stdout);
    let uid: u32 = uid_text.trim().parse().map_err(|_| {
        CistellaError::Runtime(format!("seat uid not a single integer: {uid_text:?}"))
    })?;
    // Resolve bind targets in the same batch: lexical targets cannot see
    // image-baked symlink components, so authorization compares
    // resolved-against-resolved. Order-preserved with a split index; the
    // total count must match or a line was lost and nothing is provable.
    let mut rp_args = vec!["-m".to_string(), "--".to_string()];
    rp_args.extend(candidates.iter().cloned());
    rp_args.extend(binds.iter().cloned());
    let rp_out = podman_exec(container, None, "realpath", &rp_args, timeout)?;
    let resolved: Vec<String> = String::from_utf8_lossy(&rp_out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    if resolved.len() != candidates.len() + binds.len() {
        return Err(CistellaError::Runtime(format!(
            "realpath returned {} lines for {} paths",
            resolved.len(),
            candidates.len() + binds.len()
        )));
    }
    let (resolved_cands, resolved_binds) = resolved.split_at(candidates.len());
    let mut authorized: Vec<String> = Vec::new();
    for r in resolved_cands {
        if preparation_authorized(r, resolved_binds) {
            authorized.push(r.clone());
            continue;
        }
        // Candidates are lexically clean by construction (beneath-bind
        // paths never leave `preparation_candidates`), so a resolved
        // beneath-bind path means resolution revealed a bind the strings
        // did not know — symlink alias or unprovable containment. Fail
        // closed before any mkdir/chown, never skip-and-continue.
        let offender = resolved_binds
            .iter()
            .find(|t| r == *t || is_beneath(r, t))
            .cloned()
            .unwrap_or_else(|| "<unresolvable>".to_string());
        return Err(CistellaError::Runtime(format!(
            "preparation refused: {r} resolves beneath bind mount {offender}"
        )));
    }
    if authorized.is_empty() {
        return Ok(());
    }
    // mkdir first: it guarantees presence, so the following stat sees
    // every authorized path exactly once (missing paths would fail a
    // strict stat and lose positional pairing).
    let mut mkdir_args = vec!["-p".to_string(), "--".to_string()];
    mkdir_args.extend(authorized.iter().cloned());
    podman_exec(container, Some("root"), "mkdir", &mkdir_args, timeout)?;
    let mut stat_args = vec!["-c".to_string(), "%u %n".to_string(), "--".to_string()];
    stat_args.extend(authorized.iter().cloned());
    let stat_out = podman_exec(container, Some("root"), "stat", &stat_args, timeout)?;
    let owners = parse_stat_map(&String::from_utf8_lossy(&stat_out.stdout));
    // Chown the root-owned subset: podman pre-creates missing parents as
    // root before preparation runs, so "absent before mkdir" would spare
    // exactly the dirs this step exists to fix. Converging root-owned
    // container-local ancestors seat-owned is safe (per-session upper,
    // destroyed at teardown); bind mounts and `/` are structurally
    // excluded and never chowned, and modes are preserved by chown.
    let mut chown_args = vec![uid.to_string(), "--".to_string()];
    let mut need_chown = false;
    for path in &authorized {
        if owners.get(path).is_some_and(|o| o == "0") {
            chown_args.push(path.clone());
            need_chown = true;
        }
    }
    if !need_chown {
        return Ok(());
    }
    podman_exec(container, Some("root"), "chown", &chown_args, timeout)?;
    Ok(())
}
