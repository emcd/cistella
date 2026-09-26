//! External isolator wire client (task 2.2b).
//!
//! Framework-side counterpart to the `cistella-isolator-podman`
//! guest binary: hosts the guest through [`host_external`],
//! owns the fd-channel rendezvous, and implements [`Isolator`] by
//! translating trait calls into `isolator.*` wire operations plus
//! header-bound fd bundles. Conduct drives this instead of the
//! in-process backend; the in-process [`PodmanIsolator`] stays as
//! the conformance reference and as the post-mortem inspector
//! (key-based residue checks after abnormal guest exit need no
//! living guest).
//!
//! Ordering contract per launch: the fd bundle is sent BEFORE the
//! wire op is awaited (the guest blocks in `recv_bundle` after
//! receiving the op, so awaiting the op response before sending
//! would deadlock). Replay alignment is the guest's job (consumes
//! and validates its bundle even on idempotent replay).
//!
//! The client sends its OWN process stdio descriptors
//! (stdin/stdout/stderr) for `Inherit` launches — uniform across
//! PTY and piped sessions, since descriptors (not paths) cross.
//! Originals are retained structurally: conduct holds its stdio
//! for the session lifetime.
//!
//! Concurrency: one dispatcher thread owns the guest exchange and
//! demultiplexes responses by request id, so a bounded op
//! (terminate/remove/inspect) sends while an unbounded await
//! pends — `await` never wedges the control plane. Awaiting
//! callers poll their cancel flag between reply slices; on cancel
//! they mark the id detached (late terminals drop) and return
//! detached without killing anything.

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{
    CancelFlag, Deadlines, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use crate::framework::fdpass::{BundleHeader, accept_authenticated, bind_rendezvous, send_bundle};
use crate::framework::guest::host_external;
use crate::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use crate::isolators::close::{complete_close, parse_await_outcome, wire_error};
use crate::isolators::dispatch::{Command, DISPATCH_SLICE, serve};
use crate::isolators::podman::PodmanIsolator;

/// Guest binary name resolved sibling-relative to the driver.
pub const ISOLATOR_BIN: &str = "cistella-isolator-podman";

/// Extra argv carrying the fd-rendezvous path to the guest.
const FD_SOCKET_ARG: &str = "--fd-socket";

/// Selects the reported error after a failed phase with teardown
/// attempted: the teardown error dominates when residue remains OR
/// shutdown is uncertain (unverified quiescence never collapses to
/// the original error even when the snapshot is clean); otherwise
/// the original phase error stands. Conduct's prepare arm routes
/// through here; other arms reach the same table by shape.
///
/// Precondition note: Ok+uncertain returns the original error.
/// That state is unreachable through `teardown_unit` (the uncertain
/// branch never returns clean), so the selector stays conservative
/// there by construction rather than by check.
///
/// Public for the deterministic selection pin (the branch decision
/// pins directly instead of through a live guest).
pub fn select_teardown_error(
    teardown: Result<()>,
    original: CistellaError,
    residue_ok: bool,
    uncertain: bool,
) -> CistellaError {
    match teardown {
        Err(teardown_err) if !residue_ok || uncertain => teardown_err,
        _ => original,
    }
}

/// Exit code for a startup abort: residue left behind fails (1);
/// shutdown uncertainty fails (1) even when the snapshot is clean
/// — unverified quiescence must never masquerade as a clean signal
/// exit; a FAILED release also fails (1) — the guest may have
/// entered a fatal path during release itself, so a pre-release
/// uncertainty snapshot alone is stale by construction. Otherwise
/// the signal disposition rules (128+signum). Conduct's abort path
/// routes through here; the decision pins directly instead of
/// through a live signal.
#[must_use]
pub fn abort_exit_code(
    residue_left: bool,
    uncertain_before: bool,
    release_failed: bool,
    signum: i32,
) -> i32 {
    if residue_left || uncertain_before || release_failed {
        1
    } else {
        128 + signum
    }
}

/// Recovery verdict for a failed pre-exec op: exactly one
/// bounded replacement on proven guest death (`guest_dead`
/// without `shutdown_uncertain`); a used replacement, uncertain
/// shutdown, or live-guest failure never re-execs. Conduct probes
/// this BEFORE `death_checked` — on the recovery path a located
/// unit is the expected adoptable survivor, not residue.
///
/// Public for the deterministic verdict pin (the branch decision
/// pins directly instead of through a live guest).
#[must_use]
pub fn pre_exec_recovery_verdict(
    replacement_used: bool,
    guest_dead: bool,
    shutdown_uncertain: bool,
) -> bool {
    !replacement_used && guest_dead && !shutdown_uncertain
}

/// Re-host failure verdict for the pre-exec recovery path: the
/// teardown verdict stands on a clean snapshot, while residue
/// overrides on a dirty one. A failed converge reports its
/// concrete error regardless of the snapshot (its reason is the
/// most actionable signal); a nominally successful teardown with
/// remaining residue synthesizes the residue class with the
/// re-host fault rendered in (partial cleanup or a concurrently
/// recreated path must never hide behind it). Only teardown-Ok
/// plus a clean snapshot keeps the re-host error. Deliberately
/// distinct from [`select_teardown_error`], whose Ok+dirty-keeps
/// shape serves the wire-teardown arms.
///
/// Public for the deterministic pin (Ok+dirty must name residue).
#[must_use]
pub fn rehost_failure_verdict(
    teardown: Result<()>,
    residue_ok: bool,
    rehost_error: CistellaError,
) -> CistellaError {
    match (teardown, residue_ok) {
        (Err(teardown_error), _) => teardown_error,
        (Ok(()), false) => {
            CistellaError::Contract(format!("re-host failure left residue: {rehost_error}"))
        }
        (Ok(()), true) => rehost_error,
    }
}

/// External isolator client: guest process plus fd rendezvous.
pub struct WireClient {
    /// Dispatcher command channel (all ops serialize here).
    commands: mpsc::Sender<Command>,
    /// Dispatcher thread (joined on close).
    worker: Option<std::thread::JoinHandle<()>>,
    /// Accepted fd-channel socket (bundle side).
    fd_sock: OwnedFd,
    /// Rendezvous listener (kept open for the session).
    _fd_listener: OwnedFd,
    /// Rendezvous path (removed on close).
    fd_path: PathBuf,
    /// Op budgets.
    deadlines: Deadlines,
    /// Post-mortem inspector (key queries need no living guest).
    inspector: PodmanIsolator,
    /// Attempt keys recorded before mutating calls (residue duty).
    recorded_keys: std::sync::Mutex<Vec<ReconciliationKey>>,
    /// Abnormal-exit latch: set by the dispatcher ONLY after proven
    /// shutdown (send timeout, hard send failure, stream death,
    /// protocol violation). Local send refusal (oversize) and
    /// orderly Shutdown never set it. Conduct tells a dead-or-reaped
    /// guest (residue check valid) from a live one (a located unit
    /// is expected, not residue) without string-matching error text.
    dead: Arc<AtomicBool>,
    /// Shutdown-uncertain flag: set when a fatal path's shutdown
    /// proof FAILED (the guest or a descendant may live). Mutually
    /// clarifying with `dead`: proven shutdown latches dead;
    /// unproven shutdown sets uncertain WITHOUT latching, so no
    /// keyed residue check runs beside a possible-live guest while
    /// name-based convergence still routes. Both set resolves to
    /// uncertain (safer).
    uncertain: Arc<AtomicBool>,
    /// Recorded shutdown proof failure, set alongside `uncertain`
    /// before it (any thread observing uncertain==true also observes
    /// the report). `close()` reports it as dominant; conduct reads
    /// it through [`WireClient::death_checked`].
    shutdown_report: Arc<Mutex<Option<String>>>,
}

impl WireClient {
    /// Hosts the isolator guest and connects the fd channel:
    /// bind rendezvous, spawn plus hello, accept with pid binding.
    ///
    /// The accept waits at most `deadlines.hello`, polling for
    /// both connection readiness and guest death: a guest that
    /// answers hello but never connects (or dies trying) fails
    /// with a typed error instead of wedging conduct, and the
    /// rendezvous path plus guest clean up on every refusal.
    ///
    /// # Errors
    ///
    /// Returns on discovery, spawn, hello, capability, accept, or
    /// rendezvous failure, before any unit exists.
    pub fn host(
        exe_dir: &std::path::Path,
        rendezvous_dir: &std::path::Path,
        deadlines: Deadlines,
    ) -> Result<Self> {
        Self::host_as(exe_dir, ISOLATOR_BIN, &[], rendezvous_dir, deadlines)
    }

    /// Hosts a named guest binary with extra argv (conformance uses
    /// the scripted peer; production uses [`ISOLATOR_BIN`]).
    ///
    /// # Errors
    ///
    /// Returns on discovery, spawn, hello, capability, accept, or
    /// rendezvous failure, before any unit exists.
    pub fn host_as(
        exe_dir: &std::path::Path,
        name: &str,
        extra_args: &[String],
        rendezvous_dir: &std::path::Path,
        deadlines: Deadlines,
    ) -> Result<Self> {
        let (listener, path) = bind_rendezvous(rendezvous_dir)?;
        let mut args = vec![
            FD_SOCKET_ARG.to_string(),
            path.to_string_lossy().to_string(),
        ];
        args.extend(extra_args.iter().cloned());
        let guest = host_external(exe_dir, name, &args, &["isolator".to_string()], deadlines)
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&path);
            })?;
        let pid = i32::try_from(guest.pid())
            .map_err(|_| CistellaError::Runtime("guest pid out of range".to_string()))?;
        let fd_sock = Self::accept_guest(&listener, pid, deadlines.hello).inspect_err(|_| {
            let _ = std::fs::remove_file(&path);
        })?;
        let dead = Arc::new(AtomicBool::new(false));
        let uncertain = Arc::new(AtomicBool::new(false));
        let shutdown_report = Arc::new(Mutex::new(None));
        let (commands, worker) = serve(
            guest,
            deadlines.frame_completion,
            Arc::clone(&dead),
            Arc::clone(&shutdown_report),
            Arc::clone(&uncertain),
        );
        Ok(Self {
            commands,
            worker: Some(worker),
            fd_sock,
            _fd_listener: listener,
            fd_path: path,
            deadlines,
            inspector: PodmanIsolator::new(),
            recorded_keys: std::sync::Mutex::new(Vec::new()),
            dead,
            uncertain,
            shutdown_report,
        })
    }

    /// Accepts the guest's fd-channel connection under a budget,
    /// watching for guest death meanwhile.
    ///
    /// Polls the listener in short slices until `budget`: a ready
    /// connection authenticates by pid+uid; a dead guest (signal-0
    /// reports ESRCH) fails fast instead of wedging to the budget.
    /// Either refusal leaves no listener behind (the caller owns
    /// path cleanup).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on authentication failure
    /// or guest death, `CistellaError::Runtime` past the budget.
    fn accept_guest(listener: &OwnedFd, pid: i32, budget: Duration) -> Result<OwnedFd> {
        use nix::poll::{PollFd, PollFlags, poll};
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        let deadline = Instant::now() + budget;
        loop {
            // Signal-0 probes existence only; any error means the
            // guest is gone (a zombie still answers, then the
            // budget expiry reports it — bounded either way).
            if kill(Pid::from_raw(pid), None).is_err() {
                return Err(CistellaError::Contract(
                    "guest died before fd-channel connect".to_string(),
                ));
            }
            let slice = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if slice.is_zero() {
                return Err(CistellaError::Runtime(
                    "fd-channel accept timed out".to_string(),
                ));
            }
            let wait = nix::poll::PollTimeout::try_from(slice).map_err(|_| {
                CistellaError::Runtime("fd accept timeout out of range".to_string())
            })?;
            let mut pollfds = [PollFd::new(
                {
                    use std::os::fd::AsFd;
                    listener.as_fd()
                },
                PollFlags::POLLIN,
            )];
            let ready = poll(&mut pollfds, wait)
                .map_err(|e| CistellaError::Runtime(format!("poll fd rendezvous: {e}")))?;
            if ready > 0 {
                return accept_authenticated(listener, pid);
            }
        }
    }

    /// Sends one op and blocks for its terminal response payload.
    ///
    /// # Errors
    ///
    /// Returns the guest's typed error, dispatcher failure, or
    /// `CistellaError::Contract` on malformed response envelopes.
    fn roundtrip(&self, op: &str, payload: Value, timeout: Duration) -> Result<Value> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Op {
                op: op.to_string(),
                payload,
                timeout: Some(timeout),
                reply: reply_tx,
            })
            .map_err(|_| CistellaError::Protocol("dispatcher gone".to_string()))?;
        let response = reply_rx
            .recv()
            .map_err(|_| CistellaError::Protocol("dispatcher dropped the call".to_string()))??;
        Self::redeem(op, response)
    }

    /// Redeems one terminal response payload: ok unwraps, error
    /// reconstructs, anything else (both, neither, unknown shape)
    /// refuses. Exactly one of the two tags may be present.
    ///
    /// # Errors
    ///
    /// Returns the guest's typed error or
    /// `CistellaError::Contract` on malformed envelopes.
    fn redeem(op: &str, response: Value) -> Result<Value> {
        let object = response
            .as_object()
            .ok_or_else(|| CistellaError::Contract(format!("bad {op} response: not an object")))?;
        match (object.get("ok"), object.get("error")) {
            (Some(payload), None) => Ok(payload.clone()),
            (None, Some(error)) => {
                let error = error.as_object().ok_or_else(|| {
                    CistellaError::Contract(format!("bad {op} response: error shape"))
                })?;
                let code = error
                    .get("code")
                    .and_then(|code| code.as_str())
                    .ok_or_else(|| {
                        CistellaError::Contract(format!("bad {op} response: error without code"))
                    })?;
                let message = error
                    .get("message")
                    .and_then(|message| message.as_str())
                    .ok_or_else(|| {
                        CistellaError::Contract(format!("bad {op} response: error without message"))
                    })?;
                Err(wire_error(code, message))
            }
            _ => Err(CistellaError::Contract(format!(
                "bad {op} response: exactly one of ok/error"
            ))),
        }
    }

    /// Parses a wire attestation/snapshot payload into its typed form.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on shape violations (the
    /// guest sent bytes outside its schema).
    fn parse_typed<T: for<'de> serde::Deserialize<'de>>(op: &str, value: Value) -> Result<T> {
        serde_json::from_value(value)
            .map_err(|_| CistellaError::Contract(format!("bad {op} response: shape violation")))
    }

    /// Records an attempt key before a mutating call (residue duty:
    /// [`WireClient::on_guest_death`] checks exactly these keys).
    fn record_key(&self, key: &ReconciliationKey) {
        self.recorded_keys
            .lock()
            .expect("residue key lock")
            .push(key.clone());
    }

    /// Runs the post-exit residue check after abnormal guest death:
    /// every recorded key must locate to nothing. A located unit is
    /// leftover state the dead guest cannot report (its stderr is
    /// discarded), so residue fails loudly here instead of passing
    /// silently. Conduct calls this on abnormal paths; orderly
    /// close (all units removed) finds nothing and passes.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` naming residue, or the
    /// inspector's query failure.
    pub fn on_guest_death(&self) -> Result<()> {
        let keys = self.recorded_keys.lock().expect("residue key lock").clone();
        for key in &keys {
            match self.inspector.locate(key)? {
                Some(_) => {
                    return Err(CistellaError::Contract(
                        "guest exit left unit residue".to_string(),
                    ));
                }
                None => continue,
            }
        }
        Ok(())
    }

    /// Reports whether the dispatcher proved abnormal guest exit
    /// (send timeout, hard send failure, stream death, protocol
    /// violation) with a successful shutdown/reap first. Latch,
    /// never reset: local refusal, shutdown-uncertain paths, and
    /// orderly Shutdown leave it clear.
    #[must_use]
    pub fn guest_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Reports whether a fatal path's shutdown proof FAILED (the
    /// guest or a descendant may live). Set without latching death,
    /// so no keyed residue check runs — but name-based convergence
    /// still routes, and the recorded shutdown failure dominates
    /// every report on this path.
    #[must_use]
    pub fn shutdown_uncertain(&self) -> bool {
        self.uncertain.load(Ordering::SeqCst)
    }

    /// Conduct's death gate for one wire outcome: passes successes
    /// through, and passes failures through while the guest lives.
    /// Proven death runs the residue check first — located residue
    /// dominates, else the original error stands. Shutdown
    /// uncertainty returns the RECORDED shutdown failure as
    /// dominant (no keyed scan beside a possible-live guest). No
    /// error-text matching on either side.
    pub fn death_checked<T>(&self, outcome: Result<T>) -> Result<T> {
        match outcome {
            Ok(value) => Ok(value),
            Err(error) if self.shutdown_uncertain() => Err(self.shutdown_dominant(error)),
            Err(error) if self.guest_dead() => self.on_guest_death().and(Err(error)),
            Err(error) => Err(error),
        }
    }

    /// Dominant error on the shutdown-uncertain path: the recorded
    /// shutdown proof failure when present (it names the
    /// residue-class directly), else the original error.
    fn shutdown_dominant(&self, error: CistellaError) -> CistellaError {
        match self
            .shutdown_report
            .lock()
            .expect("shutdown report lock")
            .clone()
        {
            Some(recorded) => CistellaError::Protocol(recorded),
            None => error,
        }
    }

    /// Tears down one unit through the wire: terminate then remove.
    /// On guest death the wire ops cannot run, so converge directly
    /// instead (backend calls need no living guest) and let residue
    /// decide: gone means the original death error stands, remaining
    /// means the teardown failure dominates. On shutdown uncertainty
    /// converge best-effort but NEVER clean Ok — the shutdown residue
    /// dominates even when the snapshot is clean (unverified
    /// quiescence). `lock_held` selects the
    /// lock-held converge half on paths running under the
    /// creation-window guard (the full converge re-acquires and
    /// would deadlock nested) — pinned through this method, not
    /// around it, so a swapped branch fails the pin.
    ///
    /// # Errors
    ///
    /// Returns the death-checked wire failure, or the direct
    /// teardown failure when residue remains after guest death.
    pub fn teardown_unit(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        key: &ReconciliationKey,
        container_name: &str,
        session_id: &str,
        lock_held: bool,
    ) -> Result<()> {
        let outcome = self.death_checked(
            self.terminate(handle, grace, key)
                .and_then(|_| self.remove(handle, key).map(|_| ())),
        );
        match outcome {
            Ok(()) => Ok(()),
            Err(error) => {
                if self.shutdown_uncertain() {
                    // Unverified quiescence first (both-true resolves
                    // uncertain): converge best-effort, but NEVER a
                    // clean Ok — the shutdown residue dominates even
                    // when the snapshot is clean (a surviving guest
                    // could install after the check). `error` already
                    // names the shutdown class via `death_checked`.
                    if lock_held {
                        let _ = crate::runtime::teardown_inner(container_name, session_id);
                    } else {
                        let _ = crate::runtime::teardown(container_name, session_id);
                    }
                    Err(error)
                } else if self.guest_dead() {
                    let converged = if lock_held {
                        crate::runtime::teardown_inner(container_name, session_id)
                    } else {
                        crate::runtime::teardown(container_name, session_id)
                    };
                    if let Err(teardown_err) = converged
                        && !crate::runtime::residue_gone(container_name, session_id)
                    {
                        return Err(teardown_err);
                    }
                    Err(error)
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Closes the client: stops the dispatcher, shuts the guest
    /// down, and removes the rendezvous path. Join and path
    /// removal run unconditionally on every outcome (a dead
    /// dispatcher still joins, a dead guest still loses its
    /// path); only then does the dominant error report: the live
    /// shutdown result, a RECORDED shutdown proof failure from a
    /// fatal arm (residue-class, dominates any generic termination
    /// message), or a typed termination error when no reply can
    /// arrive and nothing was recorded.
    ///
    /// # Errors
    ///
    /// Returns the shutdown/residue failure if the close could
    /// not complete cleanly.
    pub fn close(mut self) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let send_ok = self
            .commands
            .send(Command::Shutdown { reply: reply_tx })
            .is_ok();
        // No `?` before join/unlink: a worker that dies after
        // accepting Shutdown (send_ok, no reply) must still join
        // and lose its path. Nested result kept, never early
        // return.
        let worker = self.worker.take();
        let outcome = if send_ok {
            complete_close(worker, &self.fd_path, reply_rx)
        } else {
            // Dispatcher already gone: same unconditional path,
            // reported as termination rather than shutdown.
            let _ = worker.map(|worker| worker.join());
            let _ = std::fs::remove_file(&self.fd_path);
            Err(CistellaError::Protocol(
                "dispatcher already terminated".to_string(),
            ))
        };
        // A recorded shutdown proof failure dominates any generic
        // outcome: it names the residue-class directly instead of
        // a bland termination.
        match self
            .shutdown_report
            .lock()
            .expect("shutdown report lock")
            .take()
        {
            Some(recorded) => Err(CistellaError::Protocol(recorded)),
            None => outcome,
        }
    }
}

impl Isolator for WireClient {
    fn capabilities(&self) -> IsolatorCapabilities {
        // Parity with the in-process Podman backend: the wire
        // client realizes exactly what its guest does.
        PodmanIsolator::new().capabilities()
    }

    fn create(&self, spec: &CreateSpec, key: &ReconciliationKey) -> Result<UnitHandle> {
        self.record_key(key);
        let unit = UnitHandle::mint();
        let payload = self.roundtrip(
            crate::isolators::wire::OP_CREATE,
            serde_json::json!({
                "unit_handle": unit.as_str(),
                "spec": serde_json::to_value(spec).expect("spec serializes"),
                "reconciliation_key": key.as_str(),
            }),
            self.deadlines.apply,
        )?;
        let echoed = payload
            .get("unit_handle")
            .and_then(|handle| handle.as_str())
            .ok_or_else(|| CistellaError::Contract("bad create response: shape".to_string()))?;
        if echoed != unit.as_str() {
            return Err(CistellaError::Contract(
                "create echoed a different handle".to_string(),
            ));
        }
        Ok(unit)
    }

    fn initiate(&self, handle: &UnitHandle, key: &ReconciliationKey) -> Result<StartedAttestation> {
        self.record_key(key);
        let payload = self.roundtrip(
            crate::isolators::wire::OP_INITIATE,
            serde_json::json!({
                "unit_handle": handle.as_str(),
                "reconciliation_key": key.as_str(),
            }),
            self.deadlines.apply,
        )?;
        let attestation = payload
            .get("started_attestation")
            .cloned()
            .ok_or_else(|| CistellaError::Contract("bad initiate response: shape".to_string()))?;
        Self::parse_typed("initiate", attestation)
    }

    fn execute_launch(
        &self,
        handle: &UnitHandle,
        argv: &[String],
        workdir: Option<&str>,
        stdio: StdioBinding,
        key: &ReconciliationKey,
    ) -> Result<ExecutionHandle> {
        self.record_key(key);
        // Bundle first, op awaited second: the guest blocks in
        // recv_bundle after receiving the op, so awaiting the op
        // response before sending would deadlock. Originals are
        // retained structurally (conduct holds its stdio).
        let (stdin, stdout, stderr) = match &stdio {
            StdioBinding::Inherit => {
                let stdin = std::io::stdin();
                let stdout = std::io::stdout();
                let stderr = std::io::stderr();
                use std::os::fd::AsFd;
                (
                    stdin.as_fd().try_clone_to_owned(),
                    stdout.as_fd().try_clone_to_owned(),
                    stderr.as_fd().try_clone_to_owned(),
                )
            }
            StdioBinding::HeldFiles { .. } => {
                return Err(CistellaError::Contract(
                    "wire client takes Inherit only".to_string(),
                ));
            }
        };
        let (stdin, stdout, stderr) = (stdin?, stdout?, stderr?);
        let execution = ExecutionHandle::mint();
        use std::os::fd::AsFd;
        send_bundle(
            &self.fd_sock,
            &BundleHeader {
                unit_handle: handle.as_str().to_string(),
                execution_handle: execution.as_str().to_string(),
            },
            &[stdin.as_fd(), stdout.as_fd(), stderr.as_fd()],
            self.deadlines.apply,
        )?;
        let payload = self.roundtrip(
            crate::isolators::wire::OP_EXECUTE_LAUNCH,
            serde_json::json!({
                "unit_handle": handle.as_str(),
                "execution_handle": execution.as_str(),
                "argv": argv,
                "workdir": workdir,
                "reconciliation_key": key.as_str(),
            }),
            self.deadlines.apply,
        )?;
        let echoed = payload
            .get("execution_handle")
            .and_then(|handle| handle.as_str())
            .ok_or_else(|| CistellaError::Contract("bad launch response: shape".to_string()))?;
        if echoed != execution.as_str() {
            return Err(CistellaError::Contract(
                "launch echoed a different handle".to_string(),
            ));
        }
        Ok(execution)
    }

    fn await_result(
        &self,
        execution: &ExecutionHandle,
        cancel: &CancelFlag,
    ) -> Result<ExecutionOutcome> {
        // Uncapped wait with caller-side cancel polling: the
        // dispatcher demultiplexes by id while this thread watches
        // the flag between reply slices. On cancel this thread
        // drops interest and returns detached; the dispatcher's
        // late-terminal send then fails harmlessly and the entry
        // clears. Nothing is killed, nothing fabricated.
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Op {
                op: crate::framework::protocol::AWAIT_RESULT_OP.to_string(),
                payload: serde_json::json!({"execution_handle": execution.as_str()}),
                timeout: None,
                reply: reply_tx,
            })
            .map_err(|_| CistellaError::Protocol("dispatcher gone".to_string()))?;
        // Last-sent request id is dispatcher-internal; detach by
        // execution handle is not addressable — instead the caller
        // drops interest by disconnecting the reply channel: the
        // dispatcher treats a failed send as detach. Cancel then
        // returns detached immediately.
        loop {
            match reply_rx.recv_timeout(DISPATCH_SLICE) {
                Ok(response) => {
                    // Terminal responses carry the tagged envelope
                    // (`{"ok": ...}` / `{"error": ...}`), exactly
                    // like every other op: redeem strictly, then
                    // parse the nested outcome. A bare
                    // `exit_status` at top level is not the schema.
                    let outcome = Self::redeem("await_result", response?)?;
                    return parse_await_outcome(&outcome);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancel.is_cancelled() {
                        // Retire the entry explicitly so it clears
                        // now instead of lingering to harness exit;
                        // the late terminal (if any) drops on the
                        // missing entry. Nothing is killed.
                        let _ = self.commands.send(Command::DetachByExec {
                            exec: execution.as_str().to_string(),
                        });
                        return Err(CistellaError::Detached(
                            "await detached by cancellation".to_string(),
                        ));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CistellaError::Protocol(
                        "dispatcher dropped the call".to_string(),
                    ));
                }
            }
        }
    }

    fn inspect(&self, handle: &UnitHandle) -> Result<UnitSnapshot> {
        let payload = self.roundtrip(
            crate::isolators::wire::OP_INSPECT,
            serde_json::json!({"unit_handle": handle.as_str()}),
            self.deadlines.plan,
        )?;
        Self::parse_typed("inspect", payload)
    }

    fn state(&self, handle: &UnitHandle) -> Result<LifecycleState> {
        let payload = self.roundtrip(
            crate::isolators::wire::OP_STATE,
            serde_json::json!({"unit_handle": handle.as_str()}),
            self.deadlines.plan,
        )?;
        let lifecycle = payload
            .get("lifecycle")
            .and_then(|lifecycle| lifecycle.as_str())
            .ok_or_else(|| CistellaError::Contract("bad state response: shape".to_string()))?;
        serde_json::from_value(serde_json::Value::String(lifecycle.to_string())).map_err(|_| {
            CistellaError::Contract("bad state response: unknown lifecycle".to_string())
        })
    }

    fn terminate(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        key: &ReconciliationKey,
    ) -> Result<StoppedAttestation> {
        let payload = self.roundtrip(
            crate::isolators::wire::OP_TERMINATE,
            serde_json::json!({
                "unit_handle": handle.as_str(),
                "grace_ms": grace.as_millis() as u64,
                "reconciliation_key": key.as_str(),
            }),
            self.deadlines.apply,
        )?;
        let attestation = payload
            .get("stopped_attestation")
            .cloned()
            .ok_or_else(|| CistellaError::Contract("bad terminate response: shape".to_string()))?;
        Self::parse_typed("terminate", attestation)
    }

    fn remove(&self, handle: &UnitHandle, key: &ReconciliationKey) -> Result<RemovedAttestation> {
        let payload = self.roundtrip(
            crate::isolators::wire::OP_REMOVE,
            serde_json::json!({
                "unit_handle": handle.as_str(),
                "reconciliation_key": key.as_str(),
            }),
            self.deadlines.apply,
        )?;
        let attestation = payload
            .get("removed_attestation")
            .cloned()
            .ok_or_else(|| CistellaError::Contract("bad remove response: shape".to_string()))?;
        Self::parse_typed("remove", attestation)
    }

    fn converge_clean(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        key: &ReconciliationKey,
    ) -> Result<()> {
        // Mirrors the reference override (never trust `state`
        // alone): an `Absent` unit with surviving scratch is
        // residue, not convergence. `remove` is idempotent through
        // removal tombstones and owns both the unit file and
        // scratch, so it closes every state including
        // absent-with-residue.
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

    fn locate(&self, key: &ReconciliationKey) -> Result<Option<UnitHandle>> {
        // Local inspector query: needs no living guest, which is
        // exactly what post-exit residue checks require.
        self.inspector.locate(key)
    }
}
