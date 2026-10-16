//! Await-path mechanics for the Podman backend: completion freeze,
//! single-flight replay, and cancellation.
//!
//! Split from `podman.rs` (line-count doctrine): table access runs
//! through `pub(crate)` fields on [`PodmanIsolator`]; the trait
//! method delegates to [`await_result`] here.

use std::time::Duration;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{CancelFlag, ExecutionHandle};
use crate::framework::isolator::ExecutionOutcome;
use crate::framework::signals;
use crate::isolators::podman::PodmanIsolator;

/// Awaits one execution to completion and freezes its outcome.
///
/// Split from the backend (line-count doctrine): table access runs
/// through `pub(crate)` fields; the trait method in
/// [`crate::isolators::podman::PodmanIsolator`] delegates here.
pub(crate) fn await_result(
    backend: &PodmanIsolator,
    execution: &ExecutionHandle,
    cancel: &CancelFlag,
) -> Result<ExecutionOutcome> {
    loop {
        enum Step {
            Cached(ExecutionOutcome),
            Finalize(std::process::ExitStatus),
            WaitCache,
            Poll,
        }
        let claim = {
            let mut table = backend.executions.lock().expect("exec table lock");
            let pending = table.get_mut(execution).ok_or_else(|| {
                CistellaError::Contract(format!("unknown execution handle: {}", execution.as_str()))
            })?;
            if let Some(outcome) = pending.outcome.clone() {
                Step::Cached(outcome)
            } else {
                let completed = match pending.child.as_mut() {
                    None => None,
                    Some(child) => match child.try_wait() {
                        Ok(status) => status,
                        Err(_) => {
                            // Reaped elsewhere; fall back to a blocking wait.
                            Some(
                                child
                                    .wait()
                                    .map_err(|e| CistellaError::Runtime(format!("wait: {e}")))?,
                            )
                        }
                    },
                };
                match completed {
                    None => match pending.child {
                        Some(_) => Step::Poll,
                        None => Step::WaitCache,
                    },
                    Some(status) => match pending.child.take() {
                        Some(_) => Step::Finalize(status),
                        // Lost the claim race after observing
                        // completion: another awaiter owns it.
                        None => Step::WaitCache,
                    },
                }
            }
        };
        match claim {
            Step::Cached(outcome) => return Ok(outcome),
            Step::Finalize(status) => {
                let outcome = crate::isolators::corroboration::corroborated_outcome(
                    backend, execution, &status, cancel,
                );
                let mut table = backend.executions.lock().expect("exec table lock");
                // First claim wins: an already-cached outcome
                // (an earlier claimer's truth) is replayed, never
                // overwritten.
                return Ok(match table.get_mut(execution) {
                    Some(pending) => {
                        if pending.outcome.is_none() {
                            pending.outcome = Some(outcome);
                        }
                        pending
                            .outcome
                            .clone()
                            .expect("outcome stored or already cached")
                    }
                    // Entry vanished mid-flight (remove raced the
                    // claim): report the fresh outcome uncached.
                    None => outcome,
                });
            }
            Step::WaitCache => {
                return await_cached_outcome(backend, execution, cancel);
            }
            Step::Poll => {
                if cancel.is_cancelled() || cancel_signum(cancel).is_some() {
                    return Err(CistellaError::Detached(format!(
                        "await detached; execution {} stays redeemable until remove",
                        execution.as_str()
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Converts a waited child status, preferring a pending cancellation
/// disposition over the raw exit code (conduct-level signals dominate).
/// Pending cancellation signal, if any.
pub(crate) fn cancel_signum(cancel: &CancelFlag) -> Option<i32> {
    if signals::got_hup() {
        return Some(1);
    }
    if signals::got_term() {
        return Some(15);
    }
    cancel.signum()
}

/// Waits (bounded, cancel-aware) for another awaiter's claimed
/// completion to land in the cache, then replays it identically.
/// Never corroborates: exactly one corroboration runs per
/// execution, so concurrent awaiters cannot diverge.
fn await_cached_outcome(
    backend: &PodmanIsolator,
    execution: &ExecutionHandle,
    cancel: &CancelFlag,
) -> Result<ExecutionOutcome> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        {
            let table = backend.executions.lock().expect("exec table lock");
            if let Some(outcome) = table
                .get(execution)
                .and_then(|pending| pending.outcome.clone())
            {
                return Ok(outcome);
            }
            if table.get(execution).is_none() {
                return Err(CistellaError::Contract(format!(
                    "execution vanished while awaiting cache: {}",
                    execution.as_str()
                )));
            }
        }
        if cancel.is_cancelled() || cancel_signum(cancel).is_some() {
            return Err(CistellaError::Detached(format!(
                "await detached; execution {} stays redeemable until remove",
                execution.as_str()
            )));
        }
        if std::time::Instant::now() > deadline {
            return Err(CistellaError::Runtime(format!(
                "await cache never filled for execution {}",
                execution.as_str()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod replay_tests {
    use std::sync::{Arc, Barrier, mpsc};

    use crate::framework::contract::{CancelFlag, ExecutionHandle, UnitHandle};
    use crate::framework::isolator::{ExecutionOutcome, Isolator};

    use crate::isolators::podman::PodmanIsolator;

    /// Plants one execution holding an already-exited `exit 3`
    /// child. Entries live until `remove`, which this test never
    /// calls; no podman exists on test seats, so corroboration
    /// queries fail deterministically into legacy mapping.
    fn plant_completed(backend: &PodmanIsolator) -> ExecutionHandle {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 3"])
            .spawn()
            .expect("spawn sh");
        child.wait().expect("pre-complete child");
        let unit = UnitHandle::mint();
        let execution = ExecutionHandle::mint();
        backend.insert_test_execution(
            unit,
            execution.clone(),
            child,
            "cistella-race-test",
            "race-session",
        );
        execution
    }

    /// Competing awaiters observe identical outcomes: the claim-once
    /// rule (`child.take()` decides the single corroborator; losers
    /// replay the cache) freezes completion per execution.
    /// Barrier-synced starts keep the claim window honest across 20
    /// iterations; bounded collection fails loudly instead of
    /// hanging; a third await proves cache replay. This pins the
    /// freeze property and termination; divergence-sensitivity rides
    /// on the single-claim structure (one corroboration per
    /// execution by construction).
    #[test]
    fn competing_awaiters_replay_identically() {
        for _ in 0..20 {
            let backend = Arc::new(PodmanIsolator::new());
            let execution = plant_completed(&backend);
            let barrier = Arc::new(Barrier::new(3));
            let (tx, rx) = mpsc::channel();
            for _ in 0..2 {
                let backend = Arc::clone(&backend);
                let execution = execution.clone();
                let barrier = Arc::clone(&barrier);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let cancel = CancelFlag::new();
                    let outcome = backend.await_result(&execution, &cancel);
                    let _ = tx.send(
                        outcome.map(|outcome| matches!(outcome, ExecutionOutcome::Exited(3))),
                    );
                });
            }
            barrier.wait();
            drop(tx);
            let mut results = Vec::new();
            for _ in 0..2 {
                results.push(
                    rx.recv_timeout(std::time::Duration::from_secs(60))
                        .expect("awaiter answered within bound"),
                );
            }
            assert_eq!(results.len(), 2, "both awaiters answered");
            assert!(
                results
                    .iter()
                    .all(|result| result.as_ref().is_ok_and(|same| *same)),
                "identical replay under concurrency: {results:?}"
            );
            let cancel = CancelFlag::new();
            match backend.await_result(&execution, &cancel) {
                Ok(ExecutionOutcome::Exited(3)) => {}
                other => panic!("post-race await replays the frozen outcome: {other:?}"),
            }
        }
    }
}
