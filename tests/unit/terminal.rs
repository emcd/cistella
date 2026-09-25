//! Terminal-disposition pins: fatal-terminal seam, teardown-error
//! selection, and abort exit codes.
//!
//! Split from `wire_dispatch.rs` at the file-size limit. These pin
//! the shutdown-uncertainty contract end to end: proven shutdown
//! latches death, failed shutdown names uncertainty without
//! latching, teardown errors dominate on residue OR uncertainty,
//! and aborts never mask unverified quiescence as signal exits.

#[test]
fn fatal_terminal_proven_shutdown_latches() {
    // Fault-injected Ok: proven shutdown latches dead-or-reaped,
    // fails nothing (empty map), records nothing, returns None.
    // The pending-caller message shape rides the integration
    // death pins; here the state transition is the assertion.
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut pending = std::collections::HashMap::new();
    let dead = AtomicBool::new(false);
    let uncertain = AtomicBool::new(false);
    let report = Mutex::new(None);
    let uncertainty = cistella::isolators::dispatch::fatal_terminal(
        &mut pending,
        &dead,
        &uncertain,
        &report,
        "guest send failed",
        Ok(()),
    );
    assert!(
        uncertainty.is_none(),
        "proven shutdown returns no uncertainty"
    );
    assert!(dead.load(Ordering::SeqCst), "proven shutdown latches death");
    assert!(
        !uncertain.load(Ordering::SeqCst),
        "proven shutdown never sets uncertainty"
    );
    assert!(report.lock().expect("report lock").is_none());
}

#[test]
fn fatal_terminal_failed_shutdown_names_uncertainty_without_latch() {
    // Fault-injected Err (representative residue class): no death
    // latch (a survivor may live — no keyed scan may run), the
    // uncertain flag sets, the proof failure records for close(),
    // and the returned detail names the shutdown class.
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut pending = std::collections::HashMap::new();
    let dead = AtomicBool::new(false);
    let uncertain = AtomicBool::new(false);
    let report = Mutex::new(None);
    let uncertainty = cistella::isolators::dispatch::fatal_terminal(
        &mut pending,
        &dead,
        &uncertain,
        &report,
        "guest send failed",
        Err(cistella::error::CistellaError::Protocol(
            "surviving process group".to_string(),
        )),
    );
    let detail = uncertainty.expect("failed shutdown returns uncertainty");
    assert!(
        detail.contains("surviving process group"),
        "shutdown class named, got: {detail}"
    );
    assert!(
        !dead.load(Ordering::SeqCst),
        "unproven shutdown must not latch death"
    );
    assert!(
        uncertain.load(Ordering::SeqCst),
        "failed shutdown proof sets uncertainty"
    );
    let recorded = report.lock().expect("report lock").clone();
    let recorded = recorded.expect("proof failure records for close()");
    assert!(
        recorded.contains("guest send failed") && recorded.contains("surviving process group"),
        "record names reason and class, got: {recorded}"
    );
}

#[test]
fn select_teardown_error_residue_dominates() {
    // Teardown failure with residue left behind dominates the
    // original phase error (certain guest, dirty snapshot).
    use cistella::isolators::client::select_teardown_error;
    let error = select_teardown_error(
        Err(cistella::error::CistellaError::Runtime(
            "teardown failed".to_string(),
        )),
        cistella::error::CistellaError::Runtime("phase failed".to_string()),
        false,
        false,
    );
    assert!(
        error.to_string().contains("teardown failed"),
        "residue dominates, got: {error}"
    );
}

#[test]
fn select_teardown_error_uncertain_dominates_clean_snapshot() {
    // Teardown failure under shutdown uncertainty dominates EVEN
    // WHEN the snapshot is clean (unverified quiescence never
    // collapses to the original error): the retracted prepare-arm
    // hole returned the phase error here, demoting recorded
    // shutdown residue.
    use cistella::isolators::client::select_teardown_error;
    let error = select_teardown_error(
        Err(cistella::error::CistellaError::Protocol(
            "shutdown failed".to_string(),
        )),
        cistella::error::CistellaError::Runtime("phase failed".to_string()),
        true,
        true,
    );
    assert!(
        error.to_string().contains("shutdown failed"),
        "uncertainty dominates clean snapshot, got: {error}"
    );
}

#[test]
fn select_teardown_error_clean_certain_keeps_original() {
    // Certain guest, clean snapshot, failed teardown with nothing
    // left: the original phase error stands (nothing to dominate
    // with).
    use cistella::isolators::client::select_teardown_error;
    let error = select_teardown_error(
        Err(cistella::error::CistellaError::Runtime(
            "teardown failed".to_string(),
        )),
        cistella::error::CistellaError::Runtime("phase failed".to_string()),
        true,
        false,
    );
    assert!(
        error.to_string().contains("phase failed"),
        "original stands when clean and certain, got: {error}"
    );
}

#[test]
fn select_teardown_error_success_keeps_original() {
    // Clean teardown: the original phase error always stands.
    use cistella::isolators::client::select_teardown_error;
    let error = select_teardown_error(
        Ok(()),
        cistella::error::CistellaError::Runtime("phase failed".to_string()),
        false,
        true,
    );
    assert!(
        error.to_string().contains("phase failed"),
        "original stands on clean teardown, got: {error}"
    );
}

#[test]
fn abort_exit_code_residue_fails() {
    use cistella::isolators::client::abort_exit_code;
    assert_eq!(abort_exit_code(true, false, false, 15), 1);
    assert_eq!(abort_exit_code(true, true, false, 15), 1);
}

#[test]
fn abort_exit_code_uncertain_clean_snapshot_fails() {
    // Shutdown uncertainty fails even when the snapshot is clean:
    // unverified quiescence must never masquerade as a clean
    // signal exit.
    use cistella::isolators::client::abort_exit_code;
    assert_eq!(abort_exit_code(false, true, false, 15), 1);
}

#[test]
fn abort_exit_code_clean_certain_keeps_signal_disposition() {
    use cistella::isolators::client::abort_exit_code;
    assert_eq!(abort_exit_code(false, false, false, 15), 143);
    assert_eq!(abort_exit_code(false, false, false, 1), 129);
}

#[test]
fn abort_exit_code_release_failed_clean_snapshot_fails() {
    // A failed close dominates the signal disposition even when
    // the snapshot is clean and no uncertainty was observed
    // before: the guest may have entered a fatal path DURING
    // release itself, so a pre-release snapshot alone is stale.
    use cistella::isolators::client::abort_exit_code;
    assert_eq!(abort_exit_code(false, false, true, 15), 1);
}
