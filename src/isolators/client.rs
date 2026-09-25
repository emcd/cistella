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
//! receiving the op, so waiting for the op response first would
//! deadlock). Replay alignment is the guest's job (consumes and
//! validates its bundle even on idempotent replay).
//!
//! The client sends its OWN process stdio descriptors
//! (stdin/stdout/stderr) for `Inherit` launches — uniform across
//! PTY and piped sessions, since descriptors (not paths) cross.
//! Originals are retained structurally: conduct holds its stdio
//! for the session lifetime.

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::time::Duration;

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
use crate::isolators::podman::PodmanIsolator;

/// Guest binary name resolved sibling-relative to the driver.
pub const ISOLATOR_BIN: &str = "cistella-isolator-podman";

/// Extra argv carrying the fd-rendezvous path to the guest.
const FD_SOCKET_ARG: &str = "--fd-socket";

/// Reconstructs a typed error from a wire error envelope.
///
/// Codes come from the guest's `error_code` mapping; unknown codes
/// refuse rather than collapsing into a generic bucket (an
/// inventing guest fails the exchange, never negotiates new
/// semantics).
///
/// # Errors
///
/// Returns the reconstructed error, or `CistellaError::Contract`
/// on malformed error envelopes.
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
    /// Hosted guest (framed protocol side). Interior mutability:
    /// conduct owns the client single-threaded per session, and
    /// every borrow scopes to one trait call.
    guest: std::sync::Mutex<GuestHost<std::process::ChildStdout, std::process::ChildStdin>>,
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
}

impl WireClient {
    /// Hosts the isolator guest and connects the fd channel:
    /// bind rendezvous, spawn plus hello, accept with pid binding.
    /// Returns the client plus the rendezvous directory owner...
    /// the caller removes nothing; [`WireClient::close`] cleans up.
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
        let (listener, path) = bind_rendezvous(rendezvous_dir)?;
        let guest = host_external(
            exe_dir,
            ISOLATOR_BIN,
            &[
                FD_SOCKET_ARG.to_string(),
                path.to_string_lossy().to_string(),
            ],
            &["isolator".to_string()],
            deadlines,
        )
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&path);
        })?;
        let pid = i32::try_from(guest.pid())
            .map_err(|_| CistellaError::Runtime("guest pid out of range".to_string()))?;
        let fd_sock = accept_authenticated(&listener, pid).inspect_err(|_| {
            let _ = std::fs::remove_file(&path);
        })?;
        Ok(Self {
            guest: std::sync::Mutex::new(guest),
            fd_sock,
            _fd_listener: listener,
            fd_path: path,
            deadlines,
            inspector: PodmanIsolator::new(),
        })
    }

    /// Sends one op and redeems its terminal response payload.
    ///
    /// # Errors
    ///
    /// Returns the guest's typed error, or `CistellaError::Contract`
    /// on malformed response envelopes.
    fn roundtrip(&self, op: &str, payload: Value, timeout: Duration) -> Result<Value> {
        let response = self
            .guest
            .lock()
            .expect("wire client lock")
            .exchange_mut()
            .request(op, payload, timeout)?;
        let object = response
            .as_object()
            .ok_or_else(|| CistellaError::Contract(format!("bad {op} response: not an object")))?;
        if let Some(payload) = object.get("ok") {
            return Ok(payload.clone());
        }
        if let Some(error) = object.get("error").and_then(|error| error.as_object()) {
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
            return Err(wire_error(code, message));
        }
        Err(CistellaError::Contract(format!(
            "bad {op} response: neither ok nor error"
        )))
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

    /// Closes the client: shuts the guest down and removes the
    /// rendezvous path. Best-effort both halves; reports the first
    /// failure.
    ///
    /// # Errors
    ///
    /// Returns the first shutdown/cleanup failure, if any.
    pub fn close(self) -> Result<()> {
        let shutdown = self
            .guest
            .into_inner()
            .expect("wire client lock")
            .shutdown();
        let _ = std::fs::remove_file(&self.fd_path);
        shutdown
    }

    /// Checks keyed residue after abnormal guest exit: every key
    /// must locate to nothing. A located unit is leftover state
    /// the dead guest cannot report (its stderr is discarded), so
    /// residue fails loudly here instead of passing silently.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` naming the first key with
    /// surviving state, or the inspector's query failure.
    pub fn assert_no_residue(&self, keys: &[ReconciliationKey]) -> Result<()> {
        for key in keys {
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
}

impl Isolator for WireClient {
    fn capabilities(&self) -> IsolatorCapabilities {
        IsolatorCapabilities {
            backend: "podman-wire",
            supports: vec![],
        }
    }

    fn create(&self, spec: &CreateSpec, key: &ReconciliationKey) -> Result<UnitHandle> {
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
        let outcome = self
            .guest
            .lock()
            .expect("wire client lock")
            .exchange_mut()
            .request_stream(
                crate::framework::protocol::AWAIT_RESULT_OP,
                serde_json::json!({"execution_handle": execution.as_str()}),
                None,
                cancel,
            )?;
        let result = outcome.result;
        if let Some(code) = result.get("exit_status").and_then(|code| code.as_i64()) {
            let code = i32::try_from(code)
                .map_err(|_| CistellaError::Contract("bad await response: shape".to_string()))?;
            return Ok(ExecutionOutcome::Exited(code));
        }
        if let Some(signum) = result.get("signal").and_then(|signum| signum.as_i64()) {
            let signum = i32::try_from(signum)
                .map_err(|_| CistellaError::Contract("bad await response: shape".to_string()))?;
            return Ok(ExecutionOutcome::Signaled(signum));
        }
        Err(CistellaError::Contract(
            "bad await response: shape".to_string(),
        ))
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
