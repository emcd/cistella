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
            crate::isolators::dispatch::POST_TERMINAL_ERROR.to_string(),
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
/// (exit, signal, unobservable, guest error, malformed) with no
/// backend. `guest_offers` carries whether the guest negotiated
/// `await-interrupted-v1`: an `unobservable` variant arriving
/// without negotiation refuses as unsolicited (it is never decoded
/// on suspicion). Exactly ONE known top-level variant must be
/// present and the record is otherwise closed: mixed tags
/// (`exit_status` + `unobservable`), extra unknown fields, and
/// out-of-bounds values all refuse. Field bounds: observation
/// strings are capped, raw codes are i32-ranged, and a typed raw
/// `exited(0)` refuses (success never classifies unobservable).
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shapes outside the exact
/// schema, on unsolicited variants, or on out-of-bounds fields.
pub fn parse_await_outcome(value: &Value, guest_offers: bool) -> Result<ExecutionOutcome> {
    let object = value
        .as_object()
        .ok_or_else(|| CistellaError::Contract("bad await response: shape".to_string()))?;
    let known = ["exit_status", "signal", "unobservable"];
    let present = known
        .iter()
        .filter(|key| object.contains_key(**key))
        .count();
    if present != 1 || object.len() != 1 {
        return Err(CistellaError::Contract(
            "bad await response: exactly one outcome variant".to_string(),
        ));
    }
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
    if let Some(unobservable) = value.get("unobservable") {
        if !guest_offers {
            return Err(CistellaError::Contract(
                "bad await response: unsolicited unobservable variant".to_string(),
            ));
        }
        return parse_unobservable(unobservable);
    }
    Err(CistellaError::Contract(
        "bad await response: shape".to_string(),
    ))
}

/// Observation string cap: systemd state names are short; anything
/// beyond this is malformed, never truncated-into-validity.
const OBSERVATION_STRING_CAP: usize = 256;

/// Parses the `unobservable` await payload with field bounds.
/// Unknown fields/tags and out-of-bounds values refuse; the harness
/// outcome stays unobservable on every accepted shape.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on malformed payloads.
fn parse_unobservable(value: &Value) -> Result<ExecutionOutcome> {
    use crate::framework::isolator::{AbsenceObservation, RawClientResult, UnobservableOutcome};
    let shape_err = || CistellaError::Contract("bad await response: unobservable".to_string());
    let object = value.as_object().ok_or_else(shape_err)?;
    if object.len() != 2 || !object.contains_key("observation") || !object.contains_key("raw") {
        return Err(shape_err());
    }
    let observation_value = &object["observation"];
    let observation_object = observation_value.as_object().ok_or_else(shape_err)?;
    if observation_object.len() != 4
        || !observation_object.contains_key("container_exists_code")
        || !observation_object.contains_key("service_active_state")
        || !observation_object.contains_key("service_load_state")
        || !observation_object.contains_key("unit_file_present")
    {
        return Err(shape_err());
    }
    let container_exists_code = observation_object["container_exists_code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok())
        .filter(|code| *code == 0 || *code == 1)
        .ok_or_else(shape_err)?;
    let service_active_state = observation_object["service_active_state"]
        .as_str()
        .filter(|state| state.len() <= OBSERVATION_STRING_CAP)
        .ok_or_else(shape_err)?
        .to_string();
    let service_load_state = observation_object["service_load_state"]
        .as_str()
        .filter(|state| state.len() <= OBSERVATION_STRING_CAP)
        .ok_or_else(shape_err)?
        .to_string();
    let unit_file_present = observation_object["unit_file_present"]
        .as_bool()
        .ok_or_else(shape_err)?;
    let raw_value = &object["raw"];
    let raw_object = raw_value.as_object().ok_or_else(shape_err)?;
    let raw = if raw_object.len() == 1 {
        if let Some(code) = raw_object
            .get("exited")
            .and_then(|code| code.as_i64())
            .and_then(|code| i32::try_from(code).ok())
            .filter(|code| *code != 0)
        {
            RawClientResult::Exited(code)
        } else if let Some(signum) = raw_object
            .get("signaled")
            .and_then(|signum| signum.as_i64())
            .and_then(|signum| i32::try_from(signum).ok())
        {
            RawClientResult::Signaled(signum)
        } else {
            return Err(shape_err());
        }
    } else {
        return Err(shape_err());
    };
    Ok(ExecutionOutcome::Unobservable(UnobservableOutcome {
        observation: AbsenceObservation {
            container_exists_code,
            service_active_state,
            service_load_state,
            unit_file_present,
        },
        raw,
    }))
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
