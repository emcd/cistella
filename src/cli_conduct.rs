//! The `conduct` verb for the `cistella` binary: session hosting
//! orchestration (mint, install, start, exec, teardown) plus its
//! helpers.
//!
//! Bin-crate module (declared in `main.rs`, NOT part of the library):
//! moved verbatim from `main.rs`, no logic changes.

use crate::cli_util::canonical_directory;
use cistella::framework::conduct::{
    abort_startup, finalize_unobservable, host_isolator, release_client, report_release,
};
use cistella::framework::contract::{Deadlines, ReconciliationKey};
use cistella::framework::isolator::{CreateSpec, ExecutionOutcome, Isolator, StdioBinding};
use cistella::framework::prepare_select::prepare_selected_extension;
use cistella::framework::signals;
use cistella::guest_registry::Registry;
use cistella::isolators::client::WireClient;
use cistella::mount::{MountMode, MountTriple, podman_volume_args};
use cistella::profile::Profile;
use cistella::runtime::resolve_image_digest;
use cistella::session::{Session, mint_session_id, parse_cli_label};

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
    isolator: &cistella::profile::IsolatorResolution,
) -> Result<WireClient, cistella::error::CistellaError> {
    eprintln!("error: pre-exec guest death ({first_error}): retiring client and re-hosting");
    report_release(release_client(client, rendezvous_dir));
    match host_isolator(exe_dir, rendezvous_dir, isolator) {
        Ok(fresh) => Ok(fresh),
        Err(rehost_error) => {
            let teardown_result = cistella::runtime::teardown_inner(container_name, session_id);
            let residue_ok = cistella::runtime::residue_gone(container_name);
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
pub(crate) fn conduct_session(
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
    // Exit-code frame: every terminal path returns its code (or a
    // typed error) instead of calling `process::exit`, so locals —
    // notably the staging guards — always drop. The single
    // `process::exit` lives in `run`, after this scope ended.
) -> Result<i32, cistella::error::CistellaError> {
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
    // Guest selection (profile-declared): the isolator table
    // admits the backend, the extensions list admits prepare
    // guests. The seat registry loads eagerly here: absent means
    // empty (exact 0.2.0 behavior), malformed refuses every
    // conduct pre-create — including shipped-only ones — before
    // any session/runtime mutation. Shipped names resolve first;
    // registered names second; anything else refuses naming the
    // declaration. Composition (any declaration-ordered set)
    // is gated inside prepare_selected_extension.
    let registry = Registry::load()?;
    let isolator = prof.resolve_isolator(&registry)?;
    let extensions = prof.resolve_extensions(&registry)?;
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
        // Cloned: the launch-time confinement derivation re-reads
        // the canonical session directory below.
        host_source: directory.clone(),
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
    // host source. Runs after validation, before unit install — a
    // refusal leaves literally no residue.
    cistella::mount::nested_ro_preflight(&triples)?;
    // Deliberate policy migration (0.2.0): the legacy unconditional
    // token-assignments veto is removed here and replaced by lattice
    // evaluation below. Refusals change error class from Identity to
    // Contract; absence-by-default is preserved (the unacknowledged
    // compiled-default denial still refuses).
    let policy = cistella::framework::policy::PolicySet::load(None)?;
    cistella::framework::conduct::evaluate_profile_contributions(&prof, &accepted_env, &policy)?;
    let exe_dir =
        std::env::current_exe().map_err(|e| CistellaError::Runtime(format!("driver path: {e}")))?;
    let exe_dir = exe_dir.parent().ok_or_else(|| {
        CistellaError::Runtime("driver binary has no parent directory".to_string())
    })?;
    // SSH ambient selection rides positionally parallel to the
    // extensions vector (see `Profile::ssh_selection_argv`):
    // declared `ssh` is fed from seat ambient, everything else
    // gets empty argv.
    let extra_args = prof.ssh_selection_argv(&extensions)?;
    // Extension prepare: none, one, or many
    // (see `prepare_selected_extension` — undeclared means
    // the empty plan, pure Podman behavior; registered means the
    // recorded role required with hooks optional; shipped Landlock
    // keeps the declared-but-empty refusal below).
    let landlock_selected = extensions
        .iter()
        .any(|entry| matches!(entry, cistella::profile::ExtensionResolution::Landlock));
    // The prepare lease drops here on every path (quiescence was
    // proven in-runner wherever a handle came back; unproven paths
    // retain their record for stale-gc instead of returning one).
    let (evaluated, _prepare_lease) = prepare_selected_extension(
        extensions,
        exe_dir,
        &prof,
        &triples,
        &policy,
        &id,
        &cistella::lock::runtime_base(),
        &extra_args,
    )?;
    // Declared-but-empty refuses here, before staging or
    // create: a selected extension answering zero hooks must
    // never start a plain session silently (the selection is a
    // guarantee). Undeclared profiles always pass — nothing
    // was promised.
    cistella::framework::hooks::require_declared_hooks(
        landlock_selected,
        &evaluated.merged.guest_hooks,
    )?;
    // Declared cleanup roots record post-prepare (guest shut
    // down, observation stable); teardown reaps them, gc
    // reconciles orphans. Failures refuse pre-create.
    if !evaluated.merged.cleanup.is_empty() {
        let base = cistella::lock::runtime_base();
        cistella::cleanup::record_cleanup(&base, &id, &evaluated.merged.cleanup)?;
    }
    // Hook staging: each requested hook resolves to a private
    // verified copy mounted RO at the known guest path before
    // create. The guards live for the session; Drop owns cleanup
    // on every exit, and staging failure refuses pre-create.
    let mut _staged_guards = Vec::new();
    let mut staged_triples = Vec::new();
    for hook in &evaluated.merged.guest_hooks {
        let (guard, triple) = cistella::framework::registry::stage_hook_artifact(
            exe_dir,
            &id,
            hook.order,
            &hook.artifact,
        )?;
        triples.push(triple.clone());
        staged_triples.push(triple);
        _staged_guards.push(guard);
    }
    let (revised, placements, effective) = cistella::framework::hooks::prepare_conduct_topology(
        triples,
        &staged_triples,
        &evaluated.merged.placements,
        &evaluated.effective,
        &evaluated.merged.guest_hooks,
        &directory,
    )?;
    // Final joint topology gate (validate, preflight, scoped
    // pre-create) — still pre-create, no residue.
    cistella::mount_init::gate_final_topology(&revised, prof.home(), &evaluated.merged.cleanup)?;
    // Hook launch plan: confinement roots plus wrapper
    // argv compose pre-create — an untranslatable topology, a
    // session outside the confinement root, or a bad shape refuses
    // here with no unit, no guest, and only guard-owned staging
    // (which drops on return). The diagnostics pipe joins
    // post-host (a pre-host pipe would leak into the guest's
    // inherited fds and defeat EOF).
    let hook_argv: Option<Vec<String>> = if evaluated.merged.guest_hooks.is_empty() {
        None
    } else {
        Some(cistella::framework::hooks::plan_hook_argv(
            &evaluated.merged.guest_hooks,
            &placements,
            &effective,
            &directory,
            &argv,
        )?)
    };
    let volumes = podman_volume_args(&revised, prof.home(), None);
    let all_volumes = volumes;
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
    // Extension contributions append after accepted pairs
    // (deterministic suffix; central merge already refused collisions).
    // The ssh guest's SSH_AUTH_SOCK arrives here like every other
    // guest env — no driver injection, no last-append privilege.
    for (k, v) in &evaluated.merged.environment {
        env_extra.push(format!("{k}={v}"));
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
    let rendezvous_dir = std::env::temp_dir().join(format!("cistella-rdv-{id}"));
    let mut client = match host_isolator(exe_dir, &rendezvous_dir, &isolator) {
        Ok(client) => client,
        Err(error) => {
            let _ = std::fs::remove_dir(&rendezvous_dir);
            return Err(error);
        }
    };
    // Diagnostics pipe joins post-host (a pre-host pipe would leak
    // into the guest's inherited fds and defeat EOF) and pre-lock:
    // failure releases the guest first (the client has no Drop),
    // then reports — no unit exists yet, so no teardown runs.
    let hook_plan: Option<cistella::framework::hooks::HookLaunchPlan> = match hook_argv {
        None => None,
        Some(argv) => match cistella::framework::hooks::plan_hook_launch(argv) {
            Ok(plan) => Some(plan),
            Err(e) => {
                report_release(release_client(client, &rendezvous_dir));
                return Err(e);
            }
        },
    };
    let key = ReconciliationKey::generate();
    let grace = Deadlines::default().terminate_grace;
    // Creation window: lock BEFORE any unit-file creation,
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
        return Ok(128 + signum);
    }
    // Isolator create installs the unit file; on
    // failure the shared teardown converges any installed residue (a
    // failed daemon-reload leaves a unit file behind).
    let spec = CreateSpec {
        session: session.clone(),
        volumes: all_volumes.clone(),
        env: env_extra,
        labels: merged_labels,
        // Driver marker for companion-path refusal: hooked units
        // carry the Landlock label so `enter` (not a Landlock
        // descendant) refuses typed instead of bypassing.
        landlock_hooked: !evaluated.merged.guest_hooks.is_empty(),
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
                    return Ok(abort_startup(
                        client,
                        &rendezvous_dir,
                        Some(&handle),
                        &container_name,
                        &id,
                        signum,
                    ));
                }
                // Test-hook stall between install and start: a deterministic window for
                // signal-during-startup regression, polling so signals abort promptly.
                // A release marker (CISTELLA_CONDUCT_START_GATE) replaces the
                // timed wait with a causal hold when set: the test
                // creates the named file to release; a pending
                // signal aborts through the identical path as the
                // timed stall. The check repeats every 50ms with no
                // overall timeout — absence of the file never
                // advances the session; the test's own bound fails
                // the test instead. This is explicitly a test-hook
                // poll (honest description): the interval bounds
                // signal-detection latency, never the launch.
                if let Some(gate) = start_gate() {
                    loop {
                        if gate.exists() {
                            break;
                        }
                        if let Some(signum) = signals::pending_signal() {
                            drop(guard);
                            return Ok(abort_startup(
                                client,
                                &rendezvous_dir,
                                Some(&handle),
                                &container_name,
                                &id,
                                signum,
                            ));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                } else {
                    let delay = start_delay_ms();
                    let waited = std::time::Instant::now();
                    while waited.elapsed() < std::time::Duration::from_millis(delay) {
                        if let Some(signum) = signals::pending_signal() {
                            drop(guard);
                            return Ok(abort_startup(
                                client,
                                &rendezvous_dir,
                                Some(&handle),
                                &container_name,
                                &id,
                                signum,
                            ));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
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
                                &isolator,
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
                            && !cistella::runtime::residue_gone(&container_name)
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
                        &isolator,
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
                    && !cistella::runtime::residue_gone(&container_name)
                {
                    return Err(teardown_err);
                }
                return Err(terminal);
            }
        }
    };
    // Guest-context capability probe: the staged
    // wrapper reports its kernel ABI plus handled mask from inside
    // the running container. Shortfall fails pre-execute typed;
    // plain sessions skip entirely. Failure converges lock-held
    // like the preparation failure below (guard still live).
    if !evaluated.merged.guest_hooks.is_empty()
        && let Err(error) = cistella::framework::hooks::probe_landlock_wrapper(
            &container_name,
            Deadlines::default().apply,
        )
    {
        let teardown_result = client.teardown_unit(&unit, grace, &key, &container_name, &id, true);
        let residue_ok = cistella::runtime::residue_gone(&container_name);
        let uncertain = client.shutdown_uncertain();
        drop(guard);
        let error = cistella::isolators::client::select_teardown_error(
            teardown_result,
            error,
            residue_ok,
            uncertain,
        );
        report_release(release_client(client, &rendezvous_dir));
        return Err(error);
    }
    // Mountpoint preparation runs under the creation-window lock, before
    // the harness attaches. Authorization consumes the canonical emitted
    // volume targets (profile/CLI/session/credential volumes
    // alike), so no mount prefix escapes on spelling; candidates are
    // seeded from user mounts only (profile/CLI/session), so internal
    // primitives never trigger ownership changes to system ancestors.
    // Failures use the lock-held teardown form while the guard is still
    // live (re-acquiring the held lock would deadlock the no-residue
    // path); the residue decision is made before the drop.
    let volume_targets = cistella::mount::volume_targets(&all_volumes);
    let sources =
        cistella::mount::preparation_sources(&prof.mounts, &cli_triples, &worktree_target);
    // Managed bind targets (triples rooted in declared cleanup
    // roots): nested user binds beneath them are the admitted
    // feature — the offender search exempts managed space only.
    let managed = cistella::mount::managed_bind_targets(&revised, &evaluated.merged.cleanup);
    if let Err(e) = cistella::prepare::prepare_mountpoints(
        &container_name,
        &sources,
        &volume_targets,
        prof.home(),
        &managed,
    ) {
        // Lock-held converge (the guard is still live): the full
        // converge re-acquires the creation-window lock and would
        // deadlock nested, so the inner half runs here. The
        // teardown error dominates on residue OR shutdown
        // uncertainty (never demote recorded shutdown residue to
        // the prepare error on a clean snapshot).
        let teardown_result = client.teardown_unit(&unit, grace, &key, &container_name, &id, true);
        let residue_ok = cistella::runtime::residue_gone(&container_name);
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
        return Ok(abort_startup(
            client,
            &rendezvous_dir,
            Some(&unit),
            &container_name,
            &id,
            signum,
        ));
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
    // Hooked launch: the plan (wrapper argv plus
    // diagnostics pipe) composed pre-create, so this site is
    // infallible short of the launch itself — every `?` that could
    // bypass teardown already ran before the unit existed. Session
    // start gates on the wrapper's applied attestation; a failed
    // apply never execs, so teardown past a gate failure converges
    // an empty execution. A wrapper exec failure (attested
    // applied:false post-spawn) reports wrapper failure below,
    // never a harness outcome.
    // Launch-commit pre-capture (readiness attestation): the native
    // incarnation bound at publication. Foreign sessions skip
    // entirely (identity unestablishable — no readiness conferred).
    // A failed pre-capture converges exactly like a launch failure
    // below (refuse + typed teardown, nothing published).
    let backend_native = matches!(isolator, cistella::profile::IsolatorResolution::Podman);
    let pre_incarnation = if backend_native {
        match cistella::attestation::query_incarnation(&container_name) {
            Ok(incarnation) => Some(incarnation),
            Err(error) => {
                let refusal = cistella::error::CistellaError::Runtime(format!(
                    "launch incarnation unestablishable (refusing): {error}"
                ));
                if let Err(teardown_err) =
                    client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
                {
                    report_release(release_client(client, &rendezvous_dir));
                    eprintln!(
                        "error: teardown after incarnation refusal ({refusal}): {teardown_err}"
                    );
                    return Ok(1);
                }
                report_release(release_client(client, &rendezvous_dir));
                return Err(refusal);
            }
        }
    } else {
        None
    };
    let attempt = match hook_plan {
        None => client
            .death_checked(client.execute_launch(
                &unit,
                &argv,
                Some(&worktree_target),
                StdioBinding::Inherit,
                &key,
            ))
            .map(|execution| (execution, None)),
        Some(plan) => cistella::framework::hooks::run_hooked_launch(
            &client,
            &unit,
            plan,
            &worktree_target,
            &key,
        ),
    };
    let (execution, exec_failure) = match attempt {
        Ok(pair) => pair,
        Err(error) => {
            if let Err(teardown_err) =
                client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
            {
                report_release(release_client(client, &rendezvous_dir));
                eprintln!("error: teardown after launch failure ({error}): {teardown_err}");
                return Ok(1);
            }
            report_release(release_client(client, &rendezvous_dir));
            return Err(error);
        }
    };
    // Launch-commit publication (readiness attestation): the
    // pure pre/post bound decides — publish ONLY the pre-bound
    // identity on exact equality, refuse + converge with nothing
    // published otherwise (false-negative-only: no blessed false
    // positive). Foreign sessions publish an unincarnated
    // commitment (no readiness conferred).
    if backend_native {
        let post_result = cistella::attestation::query_incarnation(&container_name);
        let committed = match cistella::attestation::commit_decision(
            pre_incarnation.as_ref(),
            post_result.as_ref().ok(),
        ) {
            cistella::attestation::CommitDecision::Publish(bound) => {
                let attestation = cistella::attestation::Attestation {
                    session_id: id.clone(),
                    container_name: container_name.clone(),
                    attempt_key: key.as_str().to_string(),
                    argv: argv.clone(),
                    execution_handle: execution.as_str().to_string(),
                    incarnation: Some(bound),
                };
                cistella::attestation::publish(&cistella::lock::runtime_base(), &attestation)
            }
            cistella::attestation::CommitDecision::Refuse(reason) => {
                // Preserve the query failure detail when the
                // post-check itself errored (diagnostics, not
                // control flow — the decision is already made).
                let detail = match &post_result {
                    Err(error) => format!("{reason} (refusing): {error}"),
                    Ok(_) => format!("{reason} (refusing)"),
                };
                Err(cistella::error::CistellaError::Runtime(detail))
            }
        };
        if let Err(refusal) = committed {
            if let Err(teardown_err) =
                client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
            {
                report_release(release_client(client, &rendezvous_dir));
                eprintln!("error: teardown after incarnation refusal ({refusal}): {teardown_err}");
                return Ok(1);
            }
            report_release(release_client(client, &rendezvous_dir));
            return Err(refusal);
        }
    } else {
        // Foreign commitment (no incarnation establishable):
        // records the launch provenance without conferring
        // readiness. Write failure converges like any commit
        // failure — no silent live-unattested session.
        let attestation = cistella::attestation::Attestation {
            session_id: id.clone(),
            container_name: container_name.clone(),
            attempt_key: key.as_str().to_string(),
            argv: argv.clone(),
            execution_handle: execution.as_str().to_string(),
            incarnation: None,
        };
        if let Err(refusal) =
            cistella::attestation::publish(&cistella::lock::runtime_base(), &attestation).map_err(
                |e| {
                    cistella::error::CistellaError::Runtime(format!(
                        "foreign commitment publish failed (refusing): {e}"
                    ))
                },
            )
        {
            if let Err(teardown_err) =
                client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
            {
                report_release(release_client(client, &rendezvous_dir));
                eprintln!("error: teardown after commitment refusal ({refusal}): {teardown_err}");
                return Ok(1);
            }
            report_release(release_client(client, &rendezvous_dir));
            return Err(refusal);
        }
    }
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
                if !cistella::runtime::residue_gone(&container_name) {
                    eprintln!("error: signal teardown left residue for {container_name}");
                    return Ok(1);
                }
                return Ok(128 + signum);
            }
            Err(error) => {
                if let Err(teardown_err) =
                    client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
                {
                    report_release(release_client(client, &rendezvous_dir));
                    eprintln!("error: teardown after await failure ({error}): {teardown_err}");
                    return Ok(1);
                }
                report_release(release_client(client, &rendezvous_dir));
                return Err(error);
            }
        };
    // Transition failure: exec failed with cause, or the
    // boundary stayed ambiguous (signal in the race window —
    // operator-approved scope: exact fidelity holds only
    // post-transition). A harness outcome may exist but cannot
    // be attributed safely, so report the detail verbatim (it
    // names itself: exec failure vs ambiguity) instead of a
    // harness disposition, converging like a launch failure.
    // This arm precedes the unobservable arm on purpose: a hooked
    // failed transition owns its specific disposition even when
    // the completion is also ambiguous.
    if let Some(detail) = exec_failure {
        if let Err(teardown_err) =
            client.teardown_unit(&unit, grace, &key, &container_name, &id, false)
        {
            report_release(release_client(client, &rendezvous_dir));
            eprintln!("error: teardown after transition failure: {teardown_err}");
            return Ok(1);
        }
        report_release(release_client(client, &rendezvous_dir));
        return Err(CistellaError::Runtime(detail));
    }
    // Outcome-unobservable completion: the harness disposition is
    // unknowable, so teardown converges first and the typed
    // observation reports — never a fabricated harness signal, never
    // exit 128+N or 0. Finalization lives in framework conduct
    // (same ownership shape as `abort_startup`); proof dominance
    // (any teardown/release failure, unproven absence) inside.
    if let ExecutionOutcome::Unobservable(unobservable) = &status {
        let coords = cistella::framework::conduct::TeardownCoords {
            unit: &unit,
            grace,
            key: &key,
            container_name: container_name.as_str(),
            session_id: id.as_str(),
        };
        // Local absence proof attests only native-backend cleanup:
        // a registered guest's resources are invisible to local
        // queries, so its report refuses explicitly inside.
        return Ok(finalize_unobservable(
            client,
            &rendezvous_dir,
            &coords,
            unobservable,
            backend_native,
        ));
    }
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
        Err(teardown_err) if !cistella::runtime::residue_gone(&container_name) => {
            let harness_note = match &status {
                ExecutionOutcome::Signaled(signum) => format!("harness signaled 128+{signum}"),
                ExecutionOutcome::Exited(code) => format!("harness exited {code}"),
                // Unreachable: unobservable outcomes return above
                // after their own teardown. Covered for exhaustiveness
                // without claiming a harness disposition.
                ExecutionOutcome::Unobservable(_) => "harness outcome unobservable".to_string(),
            };
            eprintln!("error: teardown after {harness_note}: {teardown_err}");
            report_release(release_client(client, &rendezvous_dir));
            return Ok(1);
        }
        Err(death_err) if client.guest_dead() => {
            report_release(release_client(client, &rendezvous_dir));
            eprintln!("error: {death_err}");
            return Ok(1);
        }
        Err(uncertain_err) if client.shutdown_uncertain() => {
            // Unverified quiescence: the shutdown residue dominates
            // EVEN WHEN the snapshot above is clean (a surviving
            // guest could install after the check). Never a clean
            // harness disposition from this path.
            report_release(release_client(client, &rendezvous_dir));
            eprintln!("error: {uncertain_err}");
            return Ok(1);
        }
        Err(_) => {}
    }
    if let Err(error) = release_client(client, &rendezvous_dir) {
        eprintln!("error: guest release: {error}");
        return Ok(1);
    }
    // Finalize observations (records-only, post-convergence): the
    // phase creates nothing — returned notes are diagnostics.
    for note in cistella::framework::hooks::run_finalize_observations(&evaluated.merged) {
        eprintln!("finalize: {note}");
    }
    Ok(status.exit_code())
}

/// Test hook: milliseconds to stall between install and start, polling for
/// signals, opening a deterministic signal-during-startup window.
fn start_delay_ms() -> u64 {
    std::env::var("CISTELLA_CONDUCT_START_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Test hook: path of a release marker file. When set, the
/// install/start stall polls for that file instead of sleeping
/// a fixed duration: release is the test creating the file,
/// abort is a pending signal. This is explicitly a
/// test-only synchronization hook next to
/// `start_delay_ms` — a bounded-interval existence check plus
/// a per-iteration signal check, with no overall timeout and
/// no bearing on the enter/readiness policy. Absence of the
/// file never advances the session; the test's own bound
/// fails the test instead.
fn start_gate() -> Option<std::path::PathBuf> {
    std::env::var("CISTELLA_CONDUCT_START_GATE")
        .ok()
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}
