//! Cistella CLI entry point.

use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};

use cistella::cli::{Cli, Command};
use cistella::mount::{MountMode, MountTriple, podman_volume_args};
use cistella::profile::Profile;
use cistella::registry::{filter_records, list_sessions, resolve_exact};
use cistella::runtime::{
    gc_exited, generate_quadlet_unit, install_quadlet, logs_container, resolve_image_digest,
    start_quadlet, teardown,
};
use cistella::session::{Session, mint_session_id, parse_cli_label};
use clap::Parser;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::sys::wait::{WaitPidFlag, WaitStatus};

/// Set when `conduct` receives `SIGHUP`.
static GOT_HUP: AtomicBool = AtomicBool::new(false);
/// Set when `conduct` receives `SIGTERM`.
static GOT_TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_conduct_signal(signal: nix::libc::c_int) {
    if signal == nix::libc::SIGHUP {
        GOT_HUP.store(true, Ordering::SeqCst);
    } else if signal == nix::libc::SIGTERM {
        GOT_TERM.store(true, Ordering::SeqCst);
    }
}

fn main() -> std::process::ExitCode {
    // Standard CLI hygiene: Rust ignores SIGPIPE by default, which turns
    // `cistella inspect | head -1` into a panic instead of a quiet exit.
    unsafe {
        let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        let _ = sigaction(Signal::SIGPIPE, &action);
    }
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), cistella::error::CistellaError> {
    match cli.command {
        Command::Conduct {
            profile,
            session_directory,
            identity,
            labels,
            image,
            configuration_directory,
            mounts,
            project_name,
            command,
        } => conduct_session(
            &profile,
            session_directory,
            identity,
            &labels,
            image,
            configuration_directory,
            &mounts,
            project_name,
            &command,
        ),
        Command::Enter {
            id,
            directory,
            labels,
            command,
        } => {
            let record = select_exact(id.as_deref(), directory.as_deref(), &labels)?;
            let container = record.container_name;
            for (field, val) in [("session", &container)] {
                if val.contains('\n') || val.contains('\r') || val.contains('\0') {
                    return Err(cistella::error::CistellaError::Runtime(format!(
                        "{field} must not contain control characters"
                    )));
                }
            }
            let cmd: Vec<String> = if command.is_empty() {
                vec!["/bin/sh".to_string()]
            } else {
                command
            };
            let args = cistella::transport::exec_args(&container, &cmd);
            let mut child = std::process::Command::new("podman")
                .args(&args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .map_err(|e| {
                    cistella::error::CistellaError::Runtime(format!("podman exec: {e}"))
                })?;
            let status = child
                .wait()
                .map_err(|e| cistella::error::CistellaError::Runtime(format!("wait: {e}")))?;
            exit_with_status(status);
        }
        Command::Survey { directory, labels } => {
            let records = list_sessions()?;
            let parsed = parse_label_filters(&labels)?;
            let canon_dir = directory.as_deref().map(canonical_directory).transpose()?;
            let shown = filter_records(&records, canon_dir.as_deref(), &parsed);
            for r in shown {
                println!(
                    "{} {} profile={} directory={} identity={}",
                    r.container_name, r.active_state, r.profile, r.directory, r.identity
                );
            }
            Ok(())
        }
        Command::Inspect {
            id,
            directory,
            labels,
        } => {
            let record = select_exact(id.as_deref(), directory.as_deref(), &labels)?;
            println!(
                "{} profile={} directory={} identity={} image={}",
                record.container_name,
                record.profile,
                record.directory,
                record.identity,
                record.image
            );
            let logs = logs_container(&record.container_name)?;
            print!("{logs}");
            Ok(())
        }
        Command::Terminate {
            id,
            directory,
            labels,
        } => {
            // Scan-and-teardown holds the creation-window lock throughout:
            // resolving first and locking only for teardown could observe a
            // half-installed unit and reap a session being born.
            let _guard = cistella::lock::LockGuard::acquire()?;
            let record = select_exact(id.as_deref(), directory.as_deref(), &labels)?;
            cistella::runtime::teardown_inner(&record.container_name, &record.id)?;
            println!("terminate {}", record.container_name);
            Ok(())
        }
        Command::Gc => {
            let res = gc_exited()?;
            if res.reaped.is_empty() {
                println!("gc: nothing to reap");
            } else {
                for n in res.reaped {
                    println!("reaped {n}");
                }
            }
            Ok(())
        }
        Command::Check => {
            cistella::preflight::run_preflight()?;
            println!("check: ok");
            Ok(())
        }
    }
}

/// Parses and validates `--label k=v` filters (refuses `cistella.`).
fn parse_label_filters(
    labels: &[String],
) -> Result<Vec<(String, String)>, cistella::error::CistellaError> {
    labels.iter().map(|a| parse_cli_label(a)).collect()
}

/// Resolves exactly one selector against the live registry.
fn select_exact(
    id: Option<&str>,
    directory: Option<&str>,
    labels: &[String],
) -> Result<cistella::registry::SessionRecord, cistella::error::CistellaError> {
    let records = list_sessions()?;
    let parsed = parse_label_filters(labels)?;
    let canon_dir = directory.map(canonical_directory).transpose()?;
    resolve_exact(&records, id, canon_dir.as_deref(), &parsed)
}

/// Canonicalizes a host directory via longest existing prefix.
///
/// `--session-directory` host defaults to the caller's cwd; both conduct-time and
/// selector-time canonicalize the same way so equality matches.
fn canonical_directory(dir: &str) -> Result<String, cistella::error::CistellaError> {
    if !dir.starts_with('/') {
        return Err(cistella::error::CistellaError::Runtime(format!(
            "directory must be absolute: {dir}"
        )));
    }
    Ok(cistella::mount::canonicalize_host_source(dir)
        .to_string_lossy()
        .to_string())
}

/// Returns the cwd as a string for the `--session-directory` host default.
fn cwd_string() -> Result<String, cistella::error::CistellaError> {
    std::env::current_dir()
        .map_err(|e| cistella::error::CistellaError::Runtime(format!("cwd: {e}")))
        .map(|p| p.to_string_lossy().to_string())
}

/// Returns the default identity label (invoking user, else `default`).
fn default_identity() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    if !user.is_empty() {
        return user;
    }
    let out = std::process::Command::new("id")
        .arg("-un")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

/// Exits the process with a waited child's disposition (or 128+signal).
fn exit_with_status(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signal) = status.signal() {
        std::process::exit(128 + signal);
    }
    std::process::exit(status.code().unwrap_or(1));
}

/// Implements `conduct`: mint, install under lock, start, exec, teardown.
///
/// Nine parameters mirror the conduct CLI surface one-to-one; bundling
/// them would only move the fields.
#[allow(clippy::too_many_arguments)]
fn conduct_session(
    profile_ref: &str,
    session_directory: Option<String>,
    identity: Option<String>,
    labels: &[String],
    image_override: Option<String>,
    configuration_directory: Option<String>,
    cli_mounts: &[String],
    project_name_flag: Option<String>,
    command: &[String],
) -> Result<(), cistella::error::CistellaError> {
    use cistella::error::CistellaError;
    use cistella::profile::ResolutionSource;
    let source = ResolutionSource::from_host_env(configuration_directory.as_deref())?;
    let directory_raw = match session_directory {
        Some(d) => d,
        None => cwd_string()?,
    };
    let (directory_host, worktree_target) =
        cistella::mount::parse_session_directory(&directory_raw)?;
    let directory = canonical_directory(&directory_host)?;
    // Project name feeds `{{project-name}}` templates: explicit flag wins,
    // otherwise the canonical directory basename, derived lazily at
    // expansion. Computed before resolution; nothing derives from the
    // working directory by accident.
    use cistella::profile::ProjectName;
    let project = match &project_name_flag {
        Some(name) => Some(ProjectName::Explicit(name)),
        None => Some(ProjectName::DirectoryDefault(&directory)),
    };
    let (prof, digest, profile_name) = Profile::resolve_in(profile_ref, &source, project)?;
    let generic: Vec<(String, String)> = labels
        .iter()
        .map(|a| parse_cli_label(a))
        .collect::<Result<_, _>>()?;
    let image_input = image_override.as_deref().unwrap_or(&prof.image);
    // Never pull implicitly: unresolvable tags are typed refusals.
    let image = resolve_image_digest(image_input)?;
    let identity = identity.unwrap_or_else(default_identity);
    let argv: Vec<String> = if command.is_empty() {
        prof.command.clone().ok_or_else(|| {
            CistellaError::Runtime(
                "no harness argv: pass command after -- or set profile command".to_string(),
            )
        })?
    } else {
        command.to_vec()
    };
    let id = mint_session_id();
    let session = Session {
        id: id.clone(),
        directory: directory.clone(),
        profile: profile_name,
        profile_digest: digest,
        identity,
        command: argv.clone(),
        image,
        container_home: prof.container_home.clone(),
    };
    let mut triples = prof.mounts.clone();
    triples.push(MountTriple {
        host_source: directory,
        container_target: worktree_target.clone(),
        mode: MountMode::Rw,
    });
    let cli_triples: Vec<MountTriple> = cli_mounts
        .iter()
        .map(|a| cistella::mount::parse_mount_triple(a))
        .collect::<Result<_, _>>()?;
    let mut triples = cistella::mount::merge_cli_mounts(&triples, &cli_triples, &worktree_target)?;
    cistella::mount::validate_mounts(&triples, &prof.container_home)?;
    // Per-session scratch (XDG path, `/tmp` fallback; Label= tracks the id).
    let scratch_host = cistella::lock::scratch_dir(&id)
        .to_string_lossy()
        .to_string();
    triples.push(MountTriple {
        host_source: scratch_host,
        container_target: "/tmp/scratch".to_string(),
        mode: MountMode::Rw,
    });
    cistella::mount::validate_mounts(&triples, prof.home())?;
    cistella::identity::assert_no_github_token(&prof)?;
    let volumes = podman_volume_args(&triples, prof.home(), None);
    let ssh_args = cistella::identity::ssh_agent_volume_args(&prof);
    // ssh_args is mixed ["--volume", "sock:sock:ro", "-e", "SSH_AUTH_SOCK=..."]; split for Quadlet
    let mut all_volumes = volumes;
    let mut env_extra: Vec<String> = prof
        .environment
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let mut i = 0;
    while i + 1 < ssh_args.len() {
        let flag = &ssh_args[i];
        let val = &ssh_args[i + 1];
        if flag == "--volume" {
            all_volumes.push(flag.clone());
            all_volumes.push(val.clone());
        } else if flag == "-e" {
            env_extra.push(val.clone());
        }
        i += 2;
    }
    // Profile `labels` table merges under CLI `--label` (one rule, CLI wins).
    let mut merged = prof.labels.iter().collect::<Vec<_>>();
    let mut merged_labels: Vec<(String, String)> = Vec::new();
    for (k, v) in merged.drain(..) {
        merged_labels.push((k.clone(), v.clone()));
    }
    for (k, v) in &generic {
        if let Some(slot) = merged_labels.iter_mut().find(|(ek, _)| ek == k) {
            slot.1 = v.clone();
        } else {
            merged_labels.push((k.clone(), v.clone()));
        }
    }
    let unit = generate_quadlet_unit(&session, &all_volumes, &env_extra, &merged_labels)?;
    let unit_name = session.quadlet_unit_name();
    let container_name = session.container_name();
    // Trap SIGHUP/SIGTERM before the lock or any residue exists, so a
    // signal during startup tears down instead of killing us by default.
    install_conduct_handlers();
    // Creation window: lock BEFORE any unit-file or scratch creation,
    // hold through install -> start, then release before exec.
    let guard = cistella::lock::LockGuard::acquire()?;
    if let Some(signum) = pending_signal() {
        drop(guard);
        std::process::exit(128 + signum);
    }
    let scratch_path = cistella::lock::scratch_dir(&id);
    if let Err(e) = std::fs::create_dir_all(&scratch_path)
        .map_err(|e| CistellaError::Runtime(format!("create scratch: {e}")))
    {
        drop(guard);
        return Err(e);
    }
    if let Some(signum) = pending_signal() {
        drop(guard);
        abort_startup(&container_name, &id, signum, false);
    }
    if let Err(e) = install_quadlet(&unit_name, &unit) {
        drop(guard);
        // Every failure past install runs teardown so no residue remains;
        // a teardown failure with residue left behind dominates the report.
        if let Err(teardown_err) = teardown(&container_name, &id)
            && !cistella::runtime::residue_gone(&container_name, &id)
        {
            return Err(teardown_err);
        }
        return Err(e);
    }
    if let Some(signum) = pending_signal() {
        drop(guard);
        abort_startup(&container_name, &id, signum, true);
    }
    // Test-hook stall between install and start: a deterministic window for
    // signal-during-startup regression, polling so signals abort promptly.
    let delay = start_delay_ms();
    let waited = std::time::Instant::now();
    while waited.elapsed() < std::time::Duration::from_millis(delay) {
        if let Some(signum) = pending_signal() {
            drop(guard);
            abort_startup(&container_name, &id, signum, true);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if let Err(e) = start_quadlet(&unit_name) {
        drop(guard);
        if let Err(teardown_err) = teardown(&container_name, &id)
            && !cistella::runtime::residue_gone(&container_name, &id)
        {
            return Err(teardown_err);
        }
        return Err(e);
    }
    // Mountpoint preparation runs under the creation-window lock, before
    // the harness attaches. Authorization consumes the canonical emitted
    // volume targets (profile/CLI/session/scratch/credential volumes
    // alike), so no mount prefix escapes on spelling; candidates are
    // seeded from user mounts only (profile/CLI/session), so internal
    // primitives never trigger ownership changes to system ancestors.
    // Failures use the lock-held teardown form while the guard is still
    // live (re-acquiring the held lock would deadlock the no-residue
    // path); the residue decision is made before the drop.
    let volume_targets = cistella::mount::volume_targets(&all_volumes);
    let sources =
        cistella::mount::preparation_sources(&prof.mounts, &cli_triples, &worktree_target);
    if let Err(e) = cistella::prepare::prepare_mountpoints(
        &container_name,
        &sources,
        &volume_targets,
        prof.home(),
    ) {
        let teardown_result = cistella::runtime::teardown_inner(&container_name, &id);
        let residue_ok = cistella::runtime::residue_gone(&container_name, &id);
        drop(guard);
        if let Err(teardown_err) = teardown_result
            && !residue_ok
        {
            return Err(teardown_err);
        }
        return Err(e);
    }
    if let Some(signum) = pending_signal() {
        drop(guard);
        abort_startup(&container_name, &id, signum, true);
    }
    drop(guard);
    println!("conduct {id}");
    // Own the harness lifetime on the pane PTY; traps SIGHUP/SIGTERM.
    // The session runs in its worktree target (validated absolute above).
    let status = exec_harness(&container_name, &argv, &worktree_target);
    // Shared teardown converges with `terminate` from another pane: the
    // unit may already be gone, which teardown tolerates via not-found.
    // A real teardown failure (residue remains) fails the invocation even
    // when the harness succeeded, naming the harness disposition.
    if let Err(teardown_err) = teardown(&container_name, &id)
        && !cistella::runtime::residue_gone(&container_name, &id)
    {
        let harness_note = match &status {
            HarnessEnd::Signaled(signum) => format!("harness signaled 128+{signum}"),
            HarnessEnd::Exited(code) => format!("harness exited {code}"),
        };
        eprintln!("error: teardown after {harness_note}: {teardown_err}");
        std::process::exit(1);
    }
    match status {
        HarnessEnd::Signaled(signum) => std::process::exit(128 + signum),
        HarnessEnd::Exited(code) => std::process::exit(code),
    }
}

/// Installs the conduct-level SIGHUP/SIGTERM traps (flag-only handlers).
///
/// Called at the top of `conduct_session`, before the lock is acquired or
/// any residue is created, so a signal during startup tears down instead of
/// taking the default action. The harnessed child resets to default in
/// `pre_exec` so it still dies with the pane.
fn install_conduct_handlers() {
    unsafe {
        let action = SigAction::new(
            SigHandler::Handler(on_conduct_signal),
            SaFlags::empty(),
            SigSet::empty(),
        );
        let _ = sigaction(Signal::SIGHUP, &action);
        let _ = sigaction(Signal::SIGTERM, &action);
    }
}

/// Returns the pending conduct-level signal, if SIGHUP/SIGTERM arrived.
fn pending_signal() -> Option<i32> {
    if GOT_HUP.load(Ordering::SeqCst) {
        Some(1)
    } else if GOT_TERM.load(Ordering::SeqCst) {
        Some(15)
    } else {
        None
    }
}

/// Test hook: milliseconds to stall between install and start, polling for
/// signals, opening a deterministic signal-during-startup window.
fn start_delay_ms() -> u64 {
    std::env::var("CISTELLA_CONDUCT_START_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Aborts a startup after a signal: cleans installed residue and exits
/// 128+signal. The creation-window guard must already be dropped (teardown
/// acquires the lock itself). Cleanup is verified: residue left behind
/// fails the invocation (exit 1) instead of reporting a clean signal exit.
fn abort_startup(container_name: &str, session_id: &str, signum: i32, installed: bool) -> ! {
    if installed {
        let _ = teardown(container_name, session_id);
    } else if let Err(e) = cistella::runtime::remove_scratch(session_id) {
        eprintln!("error: startup abort cleanup: {e}");
    }
    if !cistella::runtime::residue_gone(container_name, session_id) {
        eprintln!("error: startup abort left residue for {container_name}");
        std::process::exit(1);
    }
    std::process::exit(128 + signum);
}

/// How the harnessed `podman exec` child ended.
enum HarnessEnd {
    /// Harness exited with a status code.
    Exited(i32),
    /// `conduct` itself was signaled while attached.
    Signaled(i32),
}

/// Runs `podman exec -i -t` with stdio inherited, polling for the child
/// while honoring `SIGHUP`/`SIGTERM` to this process.
///
/// Conduct-level traps must already be installed by the caller
/// (`conduct_session` installs before any residue exists); the child
/// resets to default in `pre_exec` so it still dies with the pane.
fn exec_harness(container: &str, argv: &[String], workdir: &str) -> HarnessEnd {
    let args = cistella::transport::exec_harness_args(container, workdir, argv);
    let mut child = match unsafe {
        std::process::Command::new("podman")
            .args(&args)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .pre_exec(|| {
                let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
                let _ = sigaction(Signal::SIGHUP, &dfl);
                let _ = sigaction(Signal::SIGTERM, &dfl);
                Ok(())
            })
            .spawn()
    } {
        Ok(child) => child,
        Err(_) => return HarnessEnd::Exited(1),
    };
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    loop {
        match nix::sys::wait::waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {}
            Ok(WaitStatus::Exited(_, code)) => {
                return signal_or_exit(code);
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                return HarnessEnd::Exited(128 + signal as i32);
            }
            Ok(_) => {}
            Err(nix::errno::Errno::ECHILD) => {
                // Reaped elsewhere; fall back to blocking wait on the handle.
                return match child.wait() {
                    Ok(status) => {
                        use std::os::unix::process::ExitStatusExt;
                        if let Some(signal) = status.signal() {
                            HarnessEnd::Exited(128 + signal)
                        } else {
                            signal_or_exit(status.code().unwrap_or(1))
                        }
                    }
                    Err(_) => HarnessEnd::Exited(1),
                };
            }
            Err(_) => {}
        }
        if GOT_HUP.load(Ordering::SeqCst) {
            let _ = nix::sys::signal::kill(pid, Signal::SIGTERM);
            // Give the child a moment, then escalate and report 128+SIGHUP.
            std::thread::sleep(std::time::Duration::from_millis(200));
            match nix::sys::wait::waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {
                    let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
                    let _ = nix::sys::wait::waitpid(pid, None);
                    return HarnessEnd::Signaled(1);
                }
                _ => return HarnessEnd::Signaled(1),
            }
        }
        if GOT_TERM.load(Ordering::SeqCst) {
            let _ = nix::sys::signal::kill(pid, Signal::SIGTERM);
            std::thread::sleep(std::time::Duration::from_millis(200));
            match nix::sys::wait::waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {
                    let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
                    let _ = nix::sys::wait::waitpid(pid, None);
                    return HarnessEnd::Signaled(15);
                }
                _ => return HarnessEnd::Signaled(15),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Prefers the conduct-level signal disposition over the harness status.
fn signal_or_exit(code: i32) -> HarnessEnd {
    if GOT_HUP.load(Ordering::SeqCst) {
        HarnessEnd::Signaled(1)
    } else if GOT_TERM.load(Ordering::SeqCst) {
        HarnessEnd::Signaled(15)
    } else {
        HarnessEnd::Exited(code)
    }
}
