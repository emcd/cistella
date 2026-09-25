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

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{
    CancelFlag, Deadlines, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use crate::framework::fdpass::{BundleHeader, accept_authenticated, bind_rendezvous, send_bundle};
use crate::framework::guest::{GuestHost, host_external};
use crate::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use crate::framework::protocol::{Envelope, PROTOCOL_MAJOR};
use crate::isolators::podman::PodmanIsolator;

/// Guest binary name resolved sibling-relative to the driver.
pub const ISOLATOR_BIN: &str = "cistella-isolator-podman";

/// Extra argv carrying the fd-rendezvous path to the guest.
const FD_SOCKET_ARG: &str = "--fd-socket";

/// Dispatcher poll cadence: stream reads never block past this
/// slice, so commands, deadlines, and detach marks stay live.
const DISPATCH_SLICE: Duration = Duration::from_millis(100);

/// Bound for a single envelope send (capped even on uncapped
/// awaits: the wait is unbounded, the write never is).
const SEND_BOUND: Duration = Duration::from_secs(30);

/// One in-flight request tracked by the dispatcher.
struct Pending {
    /// Caller reply channel.
    reply: mpsc::Sender<Result<Value>>,
    /// Expiry for bounded ops; `None` for uncapped await.
    deadline: Option<Instant>,
    /// Op name (deadline diagnostics only).
    op: String,
    /// True for await calls (pending frames kept, not forwarded).
    is_await: bool,
    /// Execution handle for await calls (detach addressing).
    exec: Option<String>,
}

/// Dispatcher commands from client threads.
enum Command {
    /// Send one op; terminal response (or expiry) goes to `reply`.
    Op {
        /// Operation name.
        op: String,
        /// Request payload.
        payload: Value,
        /// Response budget; `None` for uncapped await.
        timeout: Option<Duration>,
        /// Caller reply channel.
        reply: mpsc::Sender<Result<Value>>,
    },
    /// Shut the guest down and stop the dispatcher.
    Shutdown {
        /// Shutdown result channel.
        reply: mpsc::Sender<Result<()>>,
    },
    /// Retire one await entry by execution handle (caller
    /// cancelled and already returned detached): the late
    /// terminal drops instead of lingering to harness exit.
    /// Best-effort by exec handle, not strict-id correlation:
    /// concurrent awaits sharing one handle (replays, retries)
    /// retire together, which is safe (all callers already left).
    DetachByExec {
        /// Execution handle whose await entry retires.
        exec: String,
    },
}

/// Reconstructs a typed error from a wire error envelope.
///
/// Codes come from the guest's `error_code` mapping; unknown codes
/// refuse rather than collapsing into a generic bucket (an
/// inventing guest fails the exchange, never negotiates new
/// semantics). Messages arrive inner (prefix-free); the variant
/// constructor applies its single class prefix here, so no
/// doubling.
///
/// # Errors
///
/// Returns the reconstructed error. This function never fails;
/// malformed envelopes are rejected by the caller before it runs.
fn wire_error(code: &str, message: &str) -> CistellaError {
    match code {
        "profile" => CistellaError::Profile(message.to_string()),
        "mount" => CistellaError::Mount(message.to_string()),
        "runtime" => CistellaError::Runtime(message.to_string()),
        "transport" => CistellaError::Transport(message.to_string()),
        "contract" => CistellaError::Contract(message.to_string()),
        "protocol" => CistellaError::Protocol(message.to_string()),
        "identity" => CistellaError::Identity(message.to_string()),
        "preflight" => CistellaError::Preflight(message.to_string()),
        "lock-contended" => CistellaError::LockContended,
        "selector" => CistellaError::Selector(message.to_string()),
        "detached" => CistellaError::Detached(message.to_string()),
        "io" => CistellaError::Runtime(format!("guest io: {message}")),
        _ => CistellaError::Contract(format!("unknown guest error code: {code}")),
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
        let (commands, worker) = Self::serve(guest);
        Ok(Self {
            commands,
            worker: Some(worker),
            fd_sock,
            _fd_listener: listener,
            fd_path: path,
            deadlines,
            inspector: PodmanIsolator::new(),
            recorded_keys: std::sync::Mutex::new(Vec::new()),
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

    /// Starts the dispatcher thread owning the guest exchange.
    fn serve(
        mut guest: GuestHost<std::process::ChildStdout, std::process::ChildStdin>,
    ) -> (mpsc::Sender<Command>, std::thread::JoinHandle<()>) {
        // Negotiated ceiling (post-hello): the dispatcher refuses
        // oversize at the same bound the guest was promised, not
        // the smaller default.
        let max_frame = guest.exchange_mut().max_frame();
        let (commands_tx, commands_rx) = mpsc::channel::<Command>();
        let worker = std::thread::spawn(move || {
            let mut pending: HashMap<String, Pending> = HashMap::new();
            // Explicitly retired ids (expiry, detach): late
            // terminals with these ids drop silently. Anything
            // else unknown is a protocol violation, not patience.
            let mut retired: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut next_id: u64 = 0;
            let mut stopping = false;
            let mut stream = crate::framework::stream::StreamReader::new(max_frame);
            // Fail every pending entry with the guest-death error,
            // then stop: called exactly once on a fatal stream
            // failure (EOF, protocol, IO — never the idle slice).
            let fail_all = |pending: &mut HashMap<String, Pending>, error: &str| {
                for (_, entry) in pending.drain() {
                    let _ = entry.reply.send(Err(CistellaError::Protocol(format!(
                        "guest terminated during {}: {error}",
                        entry.op
                    ))));
                }
            };
            while !stopping {
                // Drain new commands without blocking the stream.
                while let Ok(command) = commands_rx.try_recv() {
                    match command {
                        Command::Op {
                            op,
                            payload,
                            timeout,
                            reply,
                        } => {
                            next_id += 1;
                            let id = format!("c{next_id}");
                            let envelope = Envelope {
                                protocol: PROTOCOL_MAJOR,
                                id: id.clone(),
                                op: op.clone(),
                                payload: payload.clone(),
                            };
                            let deadline = Instant::now() + timeout.unwrap_or(Duration::ZERO);
                            // Uncapped await uses no deadline; the
                            // send itself stays bounded.
                            let send_by = if timeout.is_none() {
                                Instant::now() + SEND_BOUND
                            } else {
                                deadline
                            };
                            if guest.exchange_mut().send(&envelope, send_by).is_err() {
                                let _ = reply.send(Err(CistellaError::Protocol(
                                    "guest send failed".to_string(),
                                )));
                                continue;
                            }
                            let is_await = op == crate::framework::protocol::AWAIT_RESULT_OP;
                            let exec = payload
                                .get("execution_handle")
                                .and_then(|handle| handle.as_str())
                                .map(str::to_string);
                            pending.insert(
                                id,
                                Pending {
                                    reply,
                                    deadline: timeout.map(|_| deadline),
                                    op,
                                    is_await,
                                    exec,
                                },
                            );
                        }
                        Command::DetachByExec { exec } => {
                            let retired_ids: Vec<String> = pending
                                .iter()
                                .filter(|(_, entry)| entry.exec.as_deref() == Some(&exec))
                                .map(|(id, _)| id.clone())
                                .collect();
                            for id in retired_ids {
                                pending.remove(&id);
                                retired.insert(id);
                            }
                        }
                        Command::Shutdown { reply } => {
                            let _ = reply.send(guest.shutdown());
                            stopping = true;
                            break;
                        }
                    }
                }
                if stopping {
                    break;
                }
                // One stream slice through the persistent
                // assembler: trickled frames resolve across
                // slices instead of desynchronizing.
                match stream.poll_frame(guest.exchange_mut().reader_mut(), DISPATCH_SLICE) {
                    Ok(Some(body)) => {
                        // Full envelope grammar (unknown fields,
                        // correlation, token shapes): the
                        // dispatcher never trusts a hand-parsed id
                        // match alone. The response op must equal
                        // the pending op; retired ids (expiry,
                        // detach) drop silently; anything else
                        // unsolicited fails the exchange rather
                        // than confusing later correlation.
                        let envelope = match crate::framework::protocol::parse_envelope(&body) {
                            Ok(envelope) => envelope,
                            Err(_) => {
                                fail_all(&mut pending, "malformed response frame");
                                let _ = guest.shutdown();
                                break;
                            }
                        };
                        match pending.remove(&envelope.id) {
                            Some(entry) if entry.op == envelope.op => {
                                // `{pending: true}` heartbeat: kept,
                                // never forwarded; only the terminal
                                // redeems the caller.
                                let pending_frame = envelope
                                    .payload
                                    .get("pending")
                                    .and_then(|pending| pending.as_bool())
                                    .unwrap_or(false);
                                if entry.is_await && pending_frame {
                                    // Await heartbeat: keep the
                                    // entry, forward nothing. Only
                                    // the terminal redeems the
                                    // caller.
                                    pending.insert(envelope.id, entry);
                                } else if !entry.is_await && pending_frame {
                                    let _ = entry.reply.send(Err(CistellaError::Contract(
                                        "pending frame on non-await op".to_string(),
                                    )));
                                } else {
                                    let _ = entry.reply.send(Ok(envelope.payload));
                                }
                            }
                            Some(entry) => {
                                // Op mismatch: the response names a
                                // live request but answers a
                                // different operation — fail the
                                // caller, keep no ambiguity.
                                let _ = entry.reply.send(Err(CistellaError::Contract(
                                    "response op mismatches request".to_string(),
                                )));
                            }
                            None if retired.contains(&envelope.id) => {
                                // Explicitly retired (expiry,
                                // detach): late terminal drops.
                            }
                            None => {
                                // Truly unsolicited id: fail the
                                // exchange rather than confuse later
                                // correlation.
                                fail_all(&mut pending, "unsolicited response id");
                                let _ = guest.shutdown();
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        // Idle slice: loop back for commands,
                        // deadlines, and detach marks.
                    }
                    Err(error) => {
                        if crate::framework::protocol::is_read_timeout(&error) {
                            // Idle slice (no bytes yet): loop back.
                        } else {
                            // EOF, truncation, oversize, IO: the
                            // guest is gone or speaking garbage.
                            // Fail everything pending, shut down
                            // and reap, then stop. Residue duty
                            // belongs to the callers'
                            // on_guest_death paths, which this
                            // typed error triggers.
                            fail_all(&mut pending, "guest stream failed");
                            let _ = guest.shutdown();
                            break;
                        }
                    }
                }
                // Expire bounded ops; uncapped awaits never expire.
                let now = Instant::now();
                let expired: Vec<String> = pending
                    .iter()
                    .filter(|(_, entry)| entry.deadline.is_some_and(|deadline| now >= deadline))
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in expired {
                    if let Some(entry) = pending.remove(&id) {
                        retired.insert(id.clone());
                        let _ = entry.reply.send(Err(CistellaError::Protocol(format!(
                            "{} response timed out",
                            entry.op
                        ))));
                    }
                }
            }
        });
        (commands_tx, worker)
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

    /// Closes the client: stops the dispatcher, shuts the guest
    /// down, and removes the rendezvous path. Best-effort all
    /// thirds; reports the first failure.
    ///
    /// # Errors
    ///
    /// Returns the first shutdown/cleanup failure, if any.
    pub fn close(mut self) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.commands.send(Command::Shutdown { reply: reply_tx });
        let shutdown: Result<()> = reply_rx
            .recv()
            .map_err(|_| CistellaError::Protocol("dispatcher dropped shutdown".to_string()))?;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_file(&self.fd_path);
        shutdown
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

    fn locate(&self, key: &ReconciliationKey) -> Result<Option<UnitHandle>> {
        // Local inspector query: needs no living guest, which is
        // exactly what post-exit residue checks require.
        self.inspector.locate(key)
    }
}

/// Parses an await terminal payload into its outcome.
///
/// Public so the schema mapping pins directly against fixtures
/// (exit, signal, guest error, malformed) with no backend.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shapes outside the
/// `exit_status`/`signal` schema.
pub fn parse_await_outcome(value: &Value) -> Result<ExecutionOutcome> {
    if let Some(code) = value.get("exit_status").and_then(|code| code.as_i64()) {
        let code = i32::try_from(code)
            .map_err(|_| CistellaError::Contract("bad await response: shape".to_string()))?;
        return Ok(ExecutionOutcome::Exited(code));
    }
    if let Some(signum) = value.get("signal").and_then(|signum| signum.as_i64()) {
        let signum = i32::try_from(signum)
            .map_err(|_| CistellaError::Contract("bad await response: shape".to_string()))?;
        return Ok(ExecutionOutcome::Signaled(signum));
    }
    Err(CistellaError::Contract(
        "bad await response: shape".to_string(),
    ))
}
