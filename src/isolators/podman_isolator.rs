use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

use super::{
    PendingExec, PodmanIsolator, UnitEntry, UnitRecord, classify_state, container_pidns,
    inspect_container, unit_file_present,
};
use crate::error::{CistellaError, Result};
use crate::framework::contract::{
    CancelFlag, Capability, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use crate::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use crate::isolators::quadlet::{
    container_exists, generate_quadlet_unit, install_quadlet, query_unit_props, remove_unit_file,
    start_quadlet, stop_settle,
};
use crate::operation_log;

impl Isolator for PodmanIsolator {
    fn capabilities(&self) -> IsolatorCapabilities {
        IsolatorCapabilities {
            backend: "podman-quadlet",
            supports: vec![Capability::Environment, Capability::Mounts],
        }
    }

    fn create(&self, spec: &CreateSpec, key: &ReconciliationKey) -> Result<UnitHandle> {
        operation_log::scoped("install", None, |op| self.create_inner(spec, key, op))
    }

    fn initiate(
        &self,
        handle: &UnitHandle,
        _key: &ReconciliationKey,
    ) -> Result<StartedAttestation> {
        operation_log::scoped("start", None, |op| self.initiate_inner(handle, op))
    }

    fn execute_launch(
        &self,
        handle: &UnitHandle,
        argv: &[String],
        workdir: Option<&str>,
        stdio: StdioBinding,
        _key: &ReconciliationKey,
    ) -> Result<ExecutionHandle> {
        // Session stdio routing: `Inherit` wires process stdio
        // (in-process conduct, where that IS the session PTY);
        // `HeldFiles` wires descriptors received over the
        // ancillary-fd channel (external guests, whose own stdio
        // is protocol pipes that must never carry harness bytes).
        //
        // Terminal foreground follows the routing: a harness on a
        // session PTY must run in the caller's (foreground) process
        // group — spawned from the guest's background group (kill
        // semantics), its terminal I/O would stop with
        // SIGTTIN/SIGTTOU instead of flowing. The guest passes a
        // parentage-verified conductor pgid; this side re-checks
        // terminal shape and refuses a TTY launch without one
        // (silent background launch would stall). Piped sessions
        // and in-process conduct never move (no terminal semantics,
        // or already in the caller's group).
        let foreground = match &stdio {
            StdioBinding::HeldFiles {
                stdin, conductor, ..
            } => crate::framework::isolator::foreground_join(
                crate::framework::isolator::is_session_tty(stdin),
                *conductor,
            ),
            StdioBinding::Inherit => crate::framework::isolator::ForegroundJoin::Stay,
        };
        // Join target carries all three identities pre_exec
        // verifies: our own pid (are we still the guest's child),
        // the conductor pid (whose CURRENT pgid must match the
        // capture), and the captured foreground pgid (the setpgid
        // target). Comparing the guest's own pgid here would differ
        // by design and refuse every launch.
        let join_target: Option<(u32, u32, nix::unistd::Pid)> = match foreground {
            crate::framework::isolator::ForegroundJoin::Join((conductor_pid, pgid)) => Some((
                std::process::id(),
                conductor_pid,
                nix::unistd::Pid::from_raw(pgid as i32),
            )),
            crate::framework::isolator::ForegroundJoin::Stay => None,
            crate::framework::isolator::ForegroundJoin::Refuse => {
                return Err(CistellaError::Contract(
                    "pty foreground group unavailable: no verified conductor".to_string(),
                ));
            }
        };
        let (stdin, stdout, stderr, diagnostics) = match stdio {
            StdioBinding::Inherit => (
                std::process::Stdio::inherit(),
                std::process::Stdio::inherit(),
                std::process::Stdio::inherit(),
                None,
            ),
            StdioBinding::HeldFiles {
                stdin,
                stdout,
                stderr,
                diagnostics,
                ..
            } => (
                std::process::Stdio::from(stdin),
                std::process::Stdio::from(stdout),
                std::process::Stdio::from(stderr),
                diagnostics,
            ),
        };
        let record = self.record(handle)?;
        // Hooked launch: the diagnostics write-end crosses at its
        // natural number (see `prepare_diagnostics_hook`); plain
        // launches keep the existing argv untouched. The guest copy
        // stays owned through spawn (dropped explicitly after):
        // `prepare_diagnostics_hook` only borrows it, and an early
        // drop would hand podman a closed-or-reused number.
        let mut argv = argv.to_vec();
        let diagnostics_owned = diagnostics;
        // Preservation strategy resolves before hook prep:
        // singular where advertised, plural otherwise (old
        // podman selects, never refuses); the plural
        // precondition asserts inside, every launch. The held
        // guard serializes concurrent hook spawns process-wide
        // through our spawn below (drop at fn end).
        let super::hook::HookedPreservation {
            hooked,
            guard: _hook_guard,
        } = super::hook::resolve_hooked_preservation(diagnostics_owned.as_ref(), &mut argv)?;
        let args = match (workdir, hooked) {
            (Some(target), None) => {
                crate::transport::exec_harness_args(&record.container_name, target, &argv)
            }
            (None, None) => crate::transport::exec_args(&record.container_name, &argv),
            (Some(target), Some((strategy, fd))) => crate::transport::exec_hooked_args(
                &record.container_name,
                target,
                &argv,
                strategy,
                fd,
            ),
            (None, Some((strategy, fd))) => crate::transport::exec_hooked_plain_args(
                &record.container_name,
                &argv,
                strategy,
                fd,
            ),
        };
        let child = unsafe {
            Command::new("podman")
                .args(&args)
                .stdin(stdin)
                .stdout(stdout)
                .stderr(stderr)
                .pre_exec(move || {
                    // The child resets to default so it still dies with the pane.
                    let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
                    let _ = sigaction(Signal::SIGHUP, &dfl);
                    let _ = sigaction(Signal::SIGTERM, &dfl);
                    if let Some((guest_pid, conductor_pid, caller)) = join_target {
                        // Identity, two independent questions: (1)
                        // are we still the guest's child (parent
                        // must equal the guest pid captured above);
                        // (2) is the CONDUCTOR's current pgid still
                        // the captured foreground pgid (comparing
                        // the guest's own pgid here would differ by
                        // design). Any drift refuses loudly rather
                        // than joining a wrong group; setpgid
                        // failure likewise propagates (spawn refuses
                        // typed) instead of stalling silently with
                        // SIGTTIN later.
                        let ppid = nix::unistd::getppid();
                        if ppid.as_raw() as u32 != guest_pid {
                            return Err(std::io::Error::other(
                                "pty join: reparented since capture",
                            ));
                        }
                        let conductor = nix::unistd::Pid::from_raw(conductor_pid as i32);
                        let current =
                            nix::unistd::getpgid(Some(conductor)).map_err(std::io::Error::other)?;
                        if current != caller {
                            return Err(std::io::Error::other(
                                "pty join: caller group drifted since capture",
                            ));
                        }
                        nix::unistd::setpgid(nix::unistd::Pid::from_raw(0), caller)
                            .map_err(std::io::Error::other)?;
                    }
                    if let Some((crate::transport::PreserveStrategy::Plural, diag)) = hooked {
                        // Plural: dup onto fd 3 child-side (parent
                        // fd 3 may be live); assertion proved the
                        // table clean pre-spawn.
                        super::hook::dup_plural_diag(diag)?;
                    }
                    Ok(())
                })
                .spawn()
        }
        .map_err(|e| CistellaError::Runtime(format!("podman exec: {e}")))?;
        // The guest copy closes here, after spawn: the forked podman
        // client holds its own copies for `--preserve-fd`, and the
        // container holds the preserved write end. Closing before
        // spawn would hand podman a closed-or-reused number (whose
        // CLOEXEC/write could then land on a stdio fd); closing here
        // lets the framework observe EOF once the wrapper seals.
        drop(diagnostics_owned);
        let execution = ExecutionHandle::mint();
        self.executions.lock().expect("exec table lock").insert(
            execution.clone(),
            PendingExec {
                unit: handle.clone(),
                child: Some(child),
                outcome: None,
            },
        );
        Ok(execution)
    }

    fn await_result(
        &self,
        execution: &ExecutionHandle,
        cancel: &CancelFlag,
    ) -> Result<ExecutionOutcome> {
        super::podman_await::await_result(self, execution, cancel)
    }

    fn inspect(&self, handle: &UnitHandle) -> Result<UnitSnapshot> {
        // Tombstone reports absence without backend contact: the
        // unit is gone, so identity is retained while session and
        // image details read empty.
        let record = match self.resolve(handle)? {
            UnitEntry::Live(record) => record,
            UnitEntry::Removed { container_name, .. } => {
                return Ok(UnitSnapshot {
                    unit_identity: container_name,
                    lifecycle: LifecycleState::Absent,
                    active_state: "inactive".to_string(),
                    session_id: String::new(),
                    image: String::new(),
                });
            }
        };
        let (container, session_id, image) = inspect_container(&record.container_name)?;
        let props = query_unit_props(&format!("{}.service", record.container_name));
        let active_state = props
            .get("ActiveState")
            .cloned()
            .unwrap_or_else(|| "inactive".to_string());
        let unit_present = unit_file_present(&record.unit_name);
        // Executing means THIS unit has an unreaped child, not merely
        // a retained record (outcomes replay after completion) and
        // never another unit's live child (per-unit scope: a second
        // unit's harness must not ready this unit). Predicate is
        // shared with the scope pin below.
        let executing =
            unit_has_live_child(&self.executions.lock().expect("exec table lock"), handle);
        let lifecycle = classify_state(
            Some(&active_state),
            container.as_deref(),
            unit_present,
            executing,
        );
        Ok(UnitSnapshot {
            unit_identity: record.container_name,
            lifecycle,
            active_state,
            session_id,
            image,
        })
    }

    fn state(&self, handle: &UnitHandle) -> Result<LifecycleState> {
        Ok(self.inspect(handle)?.lifecycle)
    }

    fn terminate(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        _key: &ReconciliationKey,
    ) -> Result<StoppedAttestation> {
        operation_log::scoped("terminate", None, |op| {
            self.terminate_inner(handle, grace, op)
        })
    }
    fn remove(&self, handle: &UnitHandle, _key: &ReconciliationKey) -> Result<RemovedAttestation> {
        operation_log::scoped("terminate", None, |op| self.remove_inner(handle, op))
    }

    fn locate(&self, key: &ReconciliationKey) -> Result<Option<UnitHandle>> {
        // Memory hit is a hint, not proof: verify against durable
        // state before returning, or a stale entry could mask a
        // reaped unit and greenlight a duplicate install. Tombstones
        // never satisfy the hint (removed means gone by definition);
        // an entry pointing at one falls through to the scan.
        if let Some(handle) = self.keys.lock().expect("key table lock").get(key).cloned()
            && let Ok(UnitEntry::Live(record)) = self.resolve(&handle)
        {
            let alive = container_exists(&record.container_name).unwrap_or(false)
                || unit_file_present(&record.unit_name);
            if alive {
                return Ok(Some(handle));
            }
            self.keys.lock().expect("key table lock").remove(key);
        }
        self.scan_locate(key)
    }

    fn converge_clean(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        key: &ReconciliationKey,
    ) -> Result<()> {
        // Never trust `state` alone: an `Absent` unit with a surviving
        // unit file is residue, not convergence. `remove` is idempotent
        // and owns the unit file, so it closes every
        // state including absent-with-residue.
        match self.state(handle) {
            Ok(LifecycleState::Absent) => {
                self.remove(handle, key)?;
                Ok(())
            }
            Ok(_) => {
                self.terminate(handle, grace, key)?;
                self.remove(handle, key)?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl PodmanIsolator {
    /// Install body: records the unit installed or adopted; any
    /// logging failure is silent and the outcome is unchanged.
    fn create_inner(
        &self,
        spec: &CreateSpec,
        key: &ReconciliationKey,
        op: &mut Option<operation_log::Operation>,
    ) -> Result<UnitHandle> {
        // Same-key retry replays instead of duplicating: a timed-out
        // create whose unit survived (durable key label) converges on
        // the existing resource rather than installing a second one.
        // A failed scan refuses (fail-closed) instead of installing
        // blind into an uncertain outcome.
        if let Some(existing) = self.locate(key)? {
            operation_log::record_step(
                op,
                serde_json::json!({
                    "install": "adopted",
                    "session": spec.session.id,
                }),
            );
            return Ok(existing);
        }
        let session = &spec.session;
        let container_name = session.container_name();
        let unit_name = session.quadlet_unit_name();
        // The key label bakes into the unit BEFORE install: a crash
        // between install and registration still leaves a scannable
        // binding for the replacement peer.
        let unit = generate_quadlet_unit(
            session,
            &spec.volumes,
            &spec.env,
            &spec.labels,
            Some(key.as_str()),
            spec.landlock_hooked,
        )?;
        install_quadlet(&unit_name, &unit)?;
        operation_log::record_step(
            op,
            serde_json::json!({
                "install": "installed",
                "session": spec.session.id,
                "unit": unit_name,
            }),
        );
        let handle = UnitHandle::mint();
        self.register(
            handle.clone(),
            UnitRecord {
                container_name,
                session_id: session.id.clone(),
                unit_name,
            },
            key,
        );
        Ok(handle)
    }

    /// Start body: records the unit started; any logging failure is
    /// silent and the outcome is unchanged.
    fn initiate_inner(
        &self,
        handle: &UnitHandle,
        op: &mut Option<operation_log::Operation>,
    ) -> Result<StartedAttestation> {
        let record = self.record(handle)?;
        start_quadlet(&record.unit_name)?;
        operation_log::record_step(
            op,
            serde_json::json!({
                "start": "started",
                "unit": record.unit_name,
            }),
        );
        let pidns_proof = container_pidns(&record.container_name)?;
        Ok(StartedAttestation {
            unit_identity: record.container_name,
            pidns_proof,
            ready: true,
        })
    }
    /// Stop body: records the stop; silent on logging failure.
    fn terminate_inner(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        op: &mut Option<operation_log::Operation>,
    ) -> Result<StoppedAttestation> {
        // Live records converge for real; tombstones re-check the
        // retained names (a stray recreated out-of-band still
        // settles; absence reports clean without backend records).
        let (container_name, unit_name) = match self.resolve(handle)? {
            UnitEntry::Live(record) => (record.container_name, record.unit_name),
            UnitEntry::Removed {
                container_name,
                unit_name,
                ..
            } => (container_name, unit_name),
        };
        // Absent units succeed: a missing service with no container is
        // already stopped, which is the converged state.
        if !unit_file_present(&unit_name) && !container_exists(&container_name)? {
            operation_log::record_step(
                op,
                serde_json::json!({
                    "stop": "already-stopped",
                    "container": container_name,
                }),
            );
            return Ok(StoppedAttestation {
                unit_identity: container_name,
            });
        }
        stop_settle(&container_name, grace)?;
        operation_log::record_step(
            op,
            serde_json::json!({
                "stop": "stop-settled",
                "container": container_name,
            }),
        );
        Ok(StoppedAttestation {
            unit_identity: container_name,
        })
    }
    /// Unlink body: records the stop and unit removal; silent on
    /// logging failure.
    fn remove_inner(
        &self,
        handle: &UnitHandle,
        op: &mut Option<operation_log::Operation>,
    ) -> Result<RemovedAttestation> {
        let entry = self.resolve(handle)?;
        let (container_name, session_id, unit_name) = match &entry {
            UnitEntry::Live(record) => (
                record.container_name.clone(),
                record.session_id.clone(),
                record.unit_name.clone(),
            ),
            UnitEntry::Removed {
                container_name,
                session_id,
                unit_name,
            } => (
                container_name.clone(),
                session_id.clone(),
                unit_name.clone(),
            ),
        };
        match remove_unit_file(&container_name, &unit_name) {
            Ok(outcome) => operation_log::record_step(
                op,
                serde_json::json!({
                    "unlink": "unit-removed",
                    "container": container_name,
                    "session": session_id,
                    "reload": if outcome.unlinked {
                        "ok"
                    } else {
                        "not-required"
                    },
                }),
            ),
            Err(error) => {
                operation_log::record_step(
                    op,
                    serde_json::json!({
                        "unlink": "unit-remove-failed",
                        "container": container_name,
                        "session": session_id,
                        "detail": operation_log::cap_str(&error.to_string(), 500),
                    }),
                );
                return Err(error);
            }
        };
        // Execution records die with the unit: outcomes stay
        // replayable until `remove`, never after.
        self.executions
            .lock()
            .expect("exec table lock")
            .retain(|_, pending| pending.unit != *handle);
        if matches!(entry, UnitEntry::Live(_)) {
            // Evict the live binding into a tombstone only after the
            // filesystem confirms removal. Keys evict by value (the
            // caller's key parameter is not necessarily the
            // registered one); a later locate scans durable state
            // instead of trusting this handle.
            self.keys
                .lock()
                .expect("key table lock")
                .retain(|_, bound| bound != handle);
            self.units.lock().expect("unit table lock").insert(
                handle.clone(),
                UnitEntry::Removed {
                    container_name: container_name.clone(),
                    session_id: session_id.clone(),
                    unit_name: unit_name.clone(),
                },
            );
        }
        Ok(RemovedAttestation {
            unit_identity: container_name,
        })
    }
}

/// Per-unit live-child predicate behind `inspect`'s executing
/// bit: THIS unit has an unreaped harness child (never another
/// unit's live child, never a retained outcome). Pure over the
/// table so the scope rule pins without a backend.
fn unit_has_live_child(
    executions: &std::collections::HashMap<ExecutionHandle, PendingExec>,
    handle: &UnitHandle,
) -> bool {
    executions
        .values()
        .any(|pending| pending.unit == *handle && pending.child.is_some())
}

#[cfg(test)]
mod scope_tests {
    // Inline by exception (project rule): the executions table
    // is crate-private by design, and no public interface reaches
    // the predicate without a live podman backend. Single test.
    use super::{PendingExec, UnitHandle, unit_has_live_child};
    use crate::framework::contract::ExecutionHandle;

    #[test]
    fn other_unit_executing_does_not_ready_this_unit() {
        let unit_a = UnitHandle::mint();
        let unit_b = UnitHandle::mint();
        let mut table = std::collections::HashMap::new();
        // Live harness child on A only (real short-lived sleep,
        // reaped at test end — no podman involved).
        let child = std::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep");
        table.insert(
            ExecutionHandle::mint(),
            PendingExec {
                unit: unit_a.clone(),
                child: Some(child),
                outcome: None,
            },
        );
        // Retained outcome on B (reaped child, replayable
        // result): not live, never executing.
        table.insert(
            ExecutionHandle::mint(),
            PendingExec {
                unit: unit_b.clone(),
                child: None,
                outcome: Some(crate::framework::isolator::ExecutionOutcome::Exited(0)),
            },
        );
        assert!(
            unit_has_live_child(&table, &unit_a),
            "unit with a live child reads executing"
        );
        assert!(
            !unit_has_live_child(&table, &unit_b),
            "other unit's live child must not ready this unit"
        );
        // Reap the sleep (fail-loud on wedged child).
        for pending in table.values_mut() {
            if let Some(mut child) = pending.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}
