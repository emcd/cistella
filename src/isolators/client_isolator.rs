use std::sync::mpsc;
use std::time::Duration;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{
    CancelFlag, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use crate::framework::isolator::{
    CreateSpec, ExecutionOutcome, Isolator, IsolatorCapabilities, RemovedAttestation,
    StartedAttestation, StdioBinding, StoppedAttestation, UnitSnapshot,
};
use crate::isolators::close::parse_await_outcome;
use crate::isolators::dispatch::{Command, DISPATCH_SLICE};
use crate::isolators::podman::PodmanIsolator;

use super::WireClient;

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
        self.launch_inner(handle, argv, workdir, stdio, key, None)
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
            .map_err(|_| {
                CistellaError::Protocol(crate::isolators::dispatch::POST_TERMINAL_ERROR.to_string())
            })?;
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
                    let guest_offers = self.guest_capabilities.iter().any(|capability| {
                        capability == crate::framework::protocol::AWAIT_INTERRUPTED_V1
                    });
                    return parse_await_outcome(&outcome, guest_offers);
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
                        crate::isolators::dispatch::POST_TERMINAL_ERROR.to_string(),
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
        // alone): an `Absent` unit with a surviving unit file is
        // residue, not convergence. `remove` is idempotent through
        // removal tombstones and owns the unit file, so it closes
        // every state including absent-with-residue.
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
