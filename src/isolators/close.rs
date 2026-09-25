//! Wire-client terminal helpers: close completion, await-outcome
//! parsing, and error-envelope reconstruction.
//!
//! Split from `client.rs` at the file-size limit; behavior
//! unchanged. These sit at callable seams so the close contract
//! pins directly (a dead worker injects instead of racing peer
//! timing) without spinning up a real guest binary.

use std::sync::mpsc;

use serde_json::Value;

use crate::error::{CistellaError, Result};
use crate::framework::isolator::ExecutionOutcome;

/// Completes client close after the Shutdown send: joins the
/// worker and unlinks the rendezvous path on EVERY outcome,
/// including a worker that accepted Shutdown but died before
/// replying (send_ok with a disconnected reply channel). Only
/// then does the dominant error report: the live shutdown result,
/// or a typed termination error when no reply can arrive.
///
/// Public for the deterministic close-path pin (a dead worker is
/// injected directly instead of raced against peer sleep timing).
///
/// # Errors
///
/// Returns the shutdown failure, or `CistellaError::Protocol`
/// when the dispatcher died without reporting.
pub fn complete_close(
    worker: Option<std::thread::JoinHandle<()>>,
    path: &std::path::Path,
    reply: mpsc::Receiver<Result<()>>,
) -> Result<()> {
    let shutdown = match reply.recv() {
        Ok(result) => result,
        Err(_) => Err(CistellaError::Protocol(
            "dispatcher dropped shutdown".to_string(),
        )),
    };
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    let _ = std::fs::remove_file(path);
    shutdown
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
pub(crate) fn wire_error(code: &str, message: &str) -> CistellaError {
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
