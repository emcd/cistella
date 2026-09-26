//! Cistella CLI entry point.

use cistella::cli::{Cli, Command};
use cistella::framework::contract::{Deadlines, ReconciliationKey, UnitHandle};
use cistella::framework::isolator::{CreateSpec, ExecutionOutcome, Isolator, StdioBinding};
use cistella::framework::signals;
use cistella::isolators::client::WireClient;
use cistella::isolators::podman::PodmanIsolator;
use cistella::mount::{MountMode, MountTriple, podman_volume_args};
use cistella::profile::Profile;
use cistella::registry::{filter_records, list_sessions, resolve_exact};
use cistella::runtime::{gc_exited, logs_container, resolve_image_digest};
use cistella::session::{Session, mint_session_id, parse_cli_label};
use clap::Parser;

fn main() -> std::process::ExitCode {
    // Standard CLI hygiene: Rust ignores SIGPIPE by default, which turns
    // `cistella inspect | head -1` into a panic instead of a quiet exit.
    unsafe {
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
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
            supplements,
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
            &supplements,
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
            let isolator = PodmanIsolator::new();
            let handle = isolator.adopt(&record.container_name, &record.id);
            let key = ReconciliationKey::generate();
            let grace = Deadlines::default().terminate_grace;
            isolator.terminate(&handle, grace, &key)?;
            isolator.remove(&handle, &key)?;
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

/// Releases the wire client on conduct exit paths: orderly guest
/// shutdown plus rendezvous directory removal. Returns the close
/// outcome: error paths report it to stderr while keeping their
/// primary error (a release failure there is secondary, never
/// silent), and the success path fails on it — a residue-class
/// close failure dominates a clean harness. Rendezvous-dir litter
/// reports to stderr without failing (litter, not residue).
fn release_client(
    client: WireClient,
    rendezvous_dir: &std::path::Path,
) -> Result<(), cistella::error::CistellaError> {
    let outcome = client.close();
    if let Err(error) = std::fs::remove_dir(rendezvous_dir) {
        eprintln!("error: rendezvous cleanup: {error}");
    }
    outcome
}

/// Reports a release failure on an already-failing path: the
/// primary error stays the report, but guest-shutdown residue is
/// never concealed.
fn report_release(outcome: Result<(), cistella::error::CistellaError>) {
    if let Err(error) = outcome {
        eprintln!("error: guest release: {error}");
    }
}

/// Retires a proven-dead pre-exec client and hosts a
/// replacement under the still-held creation-window guard. Close
/// (unconditional join/unlink) frees the rendezvous path, then a
/// fresh host replays the same key. The first-death context
/// reports to stderr at retire time; the second failure dominates
/// downstream. Re-host failure converges directly by name with the
/// lock-held half — the full converge would re-acquire the guard
/// and deadlock — and a residue-dominant report.
///
/// Returns the fresh client, or the terminal error after
/// converging (the caller drops the guard and reports).
fn retire_and_rehost(
    client: WireClient,
    exe_dir: &std::path::Path,
    rendezvous_dir: &std::path::Path,
    container_name: &str,
    session_id: &str,
    first_error: &cistella::error::CistellaError,
) -> Result<WireClient, cistella::error::CistellaError> {
    eprintln!("error: pre-exec guest death ({first_error}): retiring client and re-hosting");
    report_release(release_client(client, rendezvous_dir));
    match WireClient::host(exe_dir, rendezvous_dir, Deadlines::default()) {
        Ok(fresh) => Ok(fresh),
        Err(rehost_error) => {
            let teardown_result = cistella::runtime::teardown_inner(container_name, session_id);
            let residue_ok = cistella::runtime::residue_gone(container_name, session_id);
            Err(cistella::isolators::client::rehost_failure_verdict(
                teardown_result,
                residue_ok,
                rehost_error,
            ))
        }
    }
}

/// Implements `conduct`: mint, install under lock, start, exec, teardown.
///
/// Ten parameters mirror the conduct CLI surface one-to-one; bundling
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
    supplement_args: &[String],
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
    // Project name feeds `{{core:project-name}}` templates: explicit flag
    // wins, otherwise the canonical directory basename, derived lazily
    // at expansion. Computed before resolution; nothing derives from the
    // working directory by accident.
    use cistella::profile::{ProjectName, Supplements, parse_supplement_arg};
    let project = match &project_name_flag {
        Some(name) => Some(ProjectName::Explicit(name)),
        None => Some(ProjectName::DirectoryDefault(&directory)),
    };
    // Supplements feed `{{supplement:*}}` templates: parsed and key-checked
    // here, resolved lazily at expansion (unreferenced names never read).
    let mut pairs = Vec::with_capacity(supplement_args.len());
    for arg in supplement_args {
        pairs.push(parse_supplement_arg(arg)?);
    }
    let supplements = Supplements::from_pairs(pairs);
    let (prof, digest, profile_name) =
        Profile::resolve_in(profile_ref, &source, project, &supplements)?;
    // Snapshot accepted invoker env first: absent names, collisions, and
    // gate violations fail here, before any session/runtime mutation.
    // Accepted pairs are post-substitution by construction (resolution
    // already expanded templates) and never template-scanned.
    let accepted_env = prof.snapshot_acceptances()?;
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
    // Nested-under-RO availability preflight: admitted topologies start
    // only when the intermediate chain pre-exists in the RO ancestor's
    // host source. Runs after validation, before scratch creation or
    // unit install — a refusal leaves literally no residue.
    for check in cistella::mount::nested_ro_checks(&triples) {
        if let Some(missing) = cistella::mount::nested_ro_missing(&check) {
            return Err(CistellaError::Mount(format!(
                "nested mount {} under read-only {}: missing {}",
                check.descendant,
                check.ancestor,
                missing.display()
            )));
        }
    }
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
    // Deliberate policy migration (0.2.0): the legacy unconditional
    // token-assignments veto is removed here and replaced by lattice
    // evaluation below. Refusals change error class from Identity to
    // Contract; absence-by-default is preserved (the unacknowledged
    // compiled-default denial still refuses).
    let policy = cistella::framework::policy::PolicySet::load(None)?;
    cistella::framework::conduct::evaluate_profile_contributions(&prof, &accepted_env, &policy)?;
    let volumes = podman_volume_args(&triples, prof.home(), None);
    let ssh_args = cistella::identity::ssh_agent_volume_args(&prof);
    // ssh_args is mixed ["--volume", "sock:sock:ro", "-e", "SSH_AUTH_SOCK=..."]; split for Quadlet
    let mut all_volumes = volumes;
    let mut env_extra: Vec<String> = prof
        .environment_assignments
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    // Accepted pairs append in profile-list order (deterministic suffix).
    // Collision checks in snapshot_acceptances guarantee no key overlap.
    for (k, v) in &accepted_env {
        env_extra.push(format!("{k}={v}"));
    }
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
    let container_name = session.container_name();
    // Trap SIGHUP/SIGTERM before the guest or the lock exists, so a
    // signal during startup tears down instead of killing us by default.
    signals::install_conduct_handlers();
    // Production conduct drives the external guest binary (resolved
    // sibling-relative to the driver): session stdio crosses by
    // explicit descriptor passing inside the client, uniform across
    // PTY and piped sessions. The in-process backend stays as the
    // conformance reference and post-mortem inspector only.
    let exe_dir =
        std::env::current_exe().map_err(|e| CistellaError::Runtime(format!("driver path: {e}")))?;
    let exe_dir = exe_dir.parent().ok_or_else(|| {
        CistellaError::Runtime("driver binary has no parent directory".to_string())
    })?;
    let rendezvous_dir = std::env::temp_dir().join(format!("cistella-rdv-{id}"));
    let mut client = match WireClient::host(exe_dir, &rendezvous_dir, Deadlines::default()) {
        Ok(client) => client,
        Err(error) => {
            let _ = std::fs::remove_dir(&rendezvous_dir);
            return Err(error);
        }
    };
    let key = ReconciliationKey::generate();
    let grace = Deadlines::default().terminate_grace;
    // Creation window: lock BEFORE any unit-file or scratch creation,
    // hold through install -> start, then release before exec. A lock
    // failure strands the hosted guest unless released here (the
    // client has no Drop): release first, then report.
    let guard = match cistella::lock::LockGuard::acquire() {
        Ok(guard) => guard,
        Err(error) => {
            report_release(release_client(client, &rendezvous_dir));
            return Err(error);
        }
    };
    if let Some(signum) = signals::pending_signal() {
        drop(guard);
        report_release(release_client(client, &rendezvous_dir));
        std::process::exit(128 + signum);
    }
    // Isolator create installs the unit file and scratch together; on
    // failure the shared teardown converges any installed residue (a
    // failed daemon-reload leaves a unit file behind).
    let spec = CreateSpec {
        session: session.clone(),
        volumes: all_volumes.clone(),
        env: env_extra,
        labels: merged_labels,
    };
    // Pre-exec episode with a single bounded replacement:
    // create + initiate converge by key, so proven guest death
    // retires the client and replays the same key/spec once under the still-held guard. The retry
    // mints a new framework handle; the guest adopts the surviving
    // unit (handles differ, unit identity converges). No
    // `death_checked` on the pre-retry attempt: a located unit is
    // the expected survivor there, and the residue check would fail
    // it before adopt converges. Terminal paths reapply the
    // residue duty through the existing quiesce-converge shape.
    let mut replacement_used = false;
    let unit = loop {
        match client.create(&spec, &key) {
            Ok(handle) => {
                if let Some(signum) = signals::pending_signal() {
                    drop(guard);
                    abort_startup(
                        client,
                        &rendezvous_dir,
                        Some(&handle),
                        &container_name,
                        &id,
                        signum,
                    );
                }
                // Test-hook stall between install and start: a deterministic window for
                // signal-during-startup regression, polling so signals abort promptly.
                let delay = start_delay_ms();
                let waited = std::time::Instant::now();
                while waited.elapsed() < std::time::Duration::from_millis(delay) {
                    if let Some(signum) = signals::pending_signal() {
                        drop(guard);
                        abort_startup(
                            client,
                            &rendezvous_dir,
                            Some(&handle),
                            &container_name,
                            &id,
                            signum,
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                match client.initiate(&handle, &key) {
                    Ok(_) => break handle,
                    Err(error) => {
                        if cistella::isolators::client::pre_exec_recovery_verdict(
                            replacement_used,
                            client.guest_dead(),
                            client.shutdown_uncertain(),
                        ) {
                            replacement_used = true;
                            match retire_and_rehost(
                                client,
                                exe_dir,
                                &rendezvous_dir,
                                &container_name,
                                &id,
                                &error,
                            ) {
                                Ok(fresh) => {
                                    client = fresh;
                                    continue;
                                }
                                Err(terminal) => {
                                    drop(guard);
                                    return Err(terminal);
                                }
                            }
                        }
                        // Terminal initiate path: residue duty FIRST
                        // (death_checked borrows the client), then
                        // quiesce before converging, same as before: a
                        // timed-out initiate leaves the guest live with an
                        // in-flight start that must die before the residue check.
                        let terminal = client.death_checked::<()>(Err(error)).unwrap_err();
                        report_release(release_client(client, &rendezvous_dir));
                        drop(guard);
                        if let Err(teardown_err) = cistella::runtime::teardown(&container_name, &id)
                            && !cistella::runtime::residue_gone(&container_name, &id)
                        {
                            return Err(teardown_err);
                        }
                        return Err(terminal);
                    }
                }
            }
            Err(error) => {
                if cistella::isolators::client::pre_exec_recovery_verdict(
                    replacement_used,
                    client.guest_dead(),
                    client.shutdown_uncertain(),
                ) {
                    replacement_used = true;
                    match retire_and_rehost(
                        client,
                        exe_dir,
                        &rendezvous_dir,
                        &container_name,
                        &id,
                        &error,
                    ) {
                        Ok(fresh) => {
                            client = fresh;
                            continue;
                        }
                        Err(terminal) => {
                            drop(guard);
                            return Err(terminal);
                        }
                    }
                }
                // Terminal create path: residue duty FIRST
                // (death_checked borrows the client), then quiesce
                // before converging: a still-live guest with an
                // in-flight mutating op (notably a timed-out create)
                // could install past the direct teardown's residue
                // check. Shutdown first (bounded), then converge, then
                // report — the residue decision runs after the guest is
                // gone, never beside a live mutator.
                let terminal = client.death_checked::<()>(Err(error)).unwrap_err();
                report_release(release_client(client, &rendezvous_dir));
                drop(guard);
                // Every failure past install runs teardown so no residue remains;
                // a teardown failure with residue left behind dominates the report.
                if let Err(teardown_err) = cistella::runtime::teardown(&container_name, &id)
                    && !cistella::runtime::residue_gone(&container_name, &id)
                {
                    return Err(teardown_err);
                }
                return Err(terminal);
            }
        }
    };
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
        // Lock-held converge (the guard is still live): the full
        // converge re-acquires the creation-window lock and would
        // deadlock nested, so the inner half runs here. The
        // teardown error dominates on residue OR shutdown
        // uncertainty (never demote recorded shutdown residue to
        // the prepare error on a clean snapshot).
        let teardown_result = client.teardown_unit(&unit, grace, &key, &container_name, &id, true);
        let residue_ok = cistella::runtime::residue_gone(&container_name, &id);
        let uncertain = client.shutdown_uncertain();
        drop(guard);
        let error = cistella::isolators::client::select_teardown_error(
            teardown_result,
            e,
            residue_ok,
            uncertain,
        );
        report_release(release_client(client, &rendezvous_dir));
        return Err(error);
    }
    if let Some(signum) = signals::pending_signal() {
        drop(guard);
        abort_startup(
            client,
            &rendezvous_dir,
            Some(&unit),
            &container_name,
            &id,
            signum,
        );
    }
    drop(guard);
    println!("conduct {id}");
    // Own the harness lifetime on the pane PTY; launch never blocks for
    // completion and the await redeems the outcome. Cancellation
    // DETACHES (trait contract): on a conduct-level signal the await
    // returns Detached and explicit terminate below owns the kill, so
    // the observable disposition (128+signal, no residue) is unchanged
    // while the trait never kills on cancel.
    // The session runs in its worktree target (validated absolute above).
    // Launch/await failures converge through the shared teardown
    // before reporting: a dead guest converges directly (wire ops
    // cannot run without it), and the death-checked error — residue
    // dominating when the exit left units — is the report.
    let execution = match client.death_checked(client.execute_launch(
        &unit,
        &argv,
        Some(&worktree_target),
        StdioBinding::Inherit,
        &key,
    )) {
        Ok(execution) => execution,
        Err(error) => {
            if let Err(teardown_err) =
                client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
            {
                report_release(release_client(client, &rendezvous_dir));
                eprintln!("error: teardown after launch failure ({error}): {teardown_err}");
                std::process::exit(1);
            }
            report_release(release_client(client, &rendezvous_dir));
            return Err(error);
        }
    };
    let status =
        match client.death_checked(client.await_result(&execution, signals::conduct_cancel())) {
            Ok(outcome) => outcome,
            Err(cistella::error::CistellaError::Detached(_)) => {
                // Signaled while attached: terminate owns the kill,
                // then exit with the signal disposition. The SIGTERM
                // fallback covers detach-without-signal (unusual, but
                // matches the common-case disposition).
                let signum = signals::conduct_cancel().signum().unwrap_or(15);
                let _ = client.terminate(&unit, grace, &key);
                let _ = client.remove(&unit, &key);
                report_release(release_client(client, &rendezvous_dir));
                if !cistella::runtime::residue_gone(&container_name, &id) {
                    eprintln!("error: signal teardown left residue for {container_name}");
                    std::process::exit(1);
                }
                std::process::exit(128 + signum);
            }
            Err(error) => {
                if let Err(teardown_err) =
                    client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
                {
                    report_release(release_client(client, &rendezvous_dir));
                    eprintln!("error: teardown after await failure ({error}): {teardown_err}");
                    std::process::exit(1);
                }
                report_release(release_client(client, &rendezvous_dir));
                return Err(error);
            }
        };
    // Shared teardown converges with `terminate` from another pane: the
    // unit may already be gone, which idempotent terminate/remove
    // tolerate via not-found. A real teardown failure (residue remains)
    // fails the invocation even when the harness succeeded, naming the
    // harness disposition. A converged guest death reports the death
    // instead of the harness disposition (abnormal exit is never
    // silent); a live-guest wire failure with no residue keeps the
    // harness disposition. A residue-class release failure dominates
    // even a clean harness (the guest's shutdown owns group-reap
    // and pipe-EOF verification, which teardown cannot see).
    match client.teardown_unit(&unit, grace, &key, &container_name, &id, false) {
        Ok(()) => {}
        Err(teardown_err) if !cistella::runtime::residue_gone(&container_name, &id) => {
            let harness_note = match &status {
                ExecutionOutcome::Signaled(signum) => format!("harness signaled 128+{signum}"),
                ExecutionOutcome::Exited(code) => format!("harness exited {code}"),
            };
            eprintln!("error: teardown after {harness_note}: {teardown_err}");
            report_release(release_client(client, &rendezvous_dir));
            std::process::exit(1);
        }
        Err(death_err) if client.guest_dead() => {
            report_release(release_client(client, &rendezvous_dir));
            eprintln!("error: {death_err}");
            std::process::exit(1);
        }
        Err(uncertain_err) if client.shutdown_uncertain() => {
            // Unverified quiescence: the shutdown residue dominates
            // EVEN WHEN the snapshot above is clean (a surviving
            // guest could install after the check). Never a clean
            // harness disposition from this path.
            report_release(release_client(client, &rendezvous_dir));
            eprintln!("error: {uncertain_err}");
            std::process::exit(1);
        }
        Err(_) => {}
    }
    if let Err(error) = release_client(client, &rendezvous_dir) {
        eprintln!("error: guest release: {error}");
        std::process::exit(1);
    }
    std::process::exit(status.exit_code());
}

/// Test hook: milliseconds to stall between install and start, polling for
/// signals, opening a deterministic signal-during-startup window.
fn start_delay_ms() -> u64 {
    std::env::var("CISTELLA_CONDUCT_START_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Aborts a startup after a signal: converges installed residue and exits
/// 128+signal. The creation-window guard must already be dropped (the
/// isolator methods used here assume the caller held it where the moved
/// mechanics did). Cleanup is verified: residue left behind fails the
/// invocation (exit 1) instead of reporting a clean signal exit. Takes
/// the wire client by value so the guest shuts down orderly on every
/// abort path instead of orphaning.
fn abort_startup(
    client: WireClient,
    rendezvous_dir: &std::path::Path,
    handle: Option<&UnitHandle>,
    container_name: &str,
    session_id: &str,
    signum: i32,
) -> ! {
    if let Some(unit) = handle {
        let key = ReconciliationKey::generate();
        let grace = Deadlines::default().terminate_grace;
        let _ = client.terminate(unit, grace, &key);
        let _ = client.remove(unit, &key);
    } else if let Err(e) = cistella::runtime::remove_scratch(session_id) {
        eprintln!("error: startup abort cleanup: {e}");
    }
    // Read uncertainty BEFORE release consumes the client, then
    // fold the release outcome in: the guest may enter a fatal
    // path DURING release itself (after wire terminate/remove
    // succeeded), so a pre-release snapshot alone is stale by
    // construction and a failed close dominates the signal
    // disposition. The release line already names any recorded
    // failure; the code must not claim a clean signal exit.
    let uncertain_before = client.shutdown_uncertain();
    let release_outcome = release_client(client, rendezvous_dir);
    let release_failed = release_outcome.is_err();
    report_release(release_outcome);
    let residue_left = !cistella::runtime::residue_gone(container_name, session_id);
    if residue_left {
        eprintln!("error: startup abort left residue for {container_name}");
    }
    std::process::exit(cistella::isolators::client::abort_exit_code(
        residue_left,
        uncertain_before,
        release_failed,
        signum,
    ));
}
