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

#[test]
fn pre_exec_verdict_proven_first_death_recovers() {
    // The only recoverable shape: unused replacement, proven
    // death, certain shutdown. Conduct probes this BEFORE
    // death_checked — a located unit is the expected survivor.
    use cistella::isolators::client::pre_exec_recovery_verdict;
    assert!(pre_exec_recovery_verdict(false, true, false));
}

#[test]
fn pre_exec_verdict_used_replacement_never_reexecs() {
    // Single bounded replacement for the whole pre-exec episode:
    // a second death fails stop, never loops.
    use cistella::isolators::client::pre_exec_recovery_verdict;
    assert!(!pre_exec_recovery_verdict(true, true, false));
}

#[test]
fn pre_exec_verdict_uncertain_never_reexecs() {
    // Uncertain shutdown never adopts beside a possible-live
    // mutator, even with the replacement still unused.
    use cistella::isolators::client::pre_exec_recovery_verdict;
    assert!(!pre_exec_recovery_verdict(false, true, true));
    assert!(!pre_exec_recovery_verdict(false, false, true));
}

#[test]
fn pre_exec_verdict_live_guest_never_reexecs() {
    // A live-guest op failure is reported, not recovered: the
    // guest still owns its tables and the error stands.
    use cistella::isolators::client::pre_exec_recovery_verdict;
    assert!(!pre_exec_recovery_verdict(false, false, false));
}

#[test]
fn rehost_failure_err_dominates_regardless_of_residue() {
    // A failed converge reports its concrete reason on either
    // snapshot: its error carries why cleanup failed and is the
    // most actionable signal.
    use cistella::error::CistellaError;
    use cistella::isolators::client::rehost_failure_verdict;
    let original = || CistellaError::Runtime("re-host refused".to_string());
    for residue_ok in [false, true] {
        let teardown_err = CistellaError::Protocol("kill_group".to_string());
        let reported =
            rehost_failure_verdict(Err(teardown_err), residue_ok, original()).to_string();
        assert_eq!(
            reported, "protocol: kill_group",
            "teardown error dominates, got: {reported}"
        );
    }
}

#[test]
fn rehost_failure_ok_dirty_synthesizes_residue() {
    // The essential gate: teardown-Ok with remaining residue must
    // NEVER report the plain re-host error — the residue class
    // dominates with the re-host fault rendered in.
    use cistella::error::CistellaError;
    use cistella::isolators::client::rehost_failure_verdict;
    let original = CistellaError::Runtime("re-host refused".to_string());
    let reported = rehost_failure_verdict(Ok(()), false, original).to_string();
    assert!(
        reported.contains("contract: re-host failure left residue"),
        "residue class dominates, got: {reported}"
    );
    assert!(
        reported.contains("runtime: re-host refused"),
        "re-host fault retained in context, got: {reported}"
    );
}

#[test]
fn rehost_failure_ok_clean_keeps_original() {
    // No residue, no teardown error: the re-host fault stands alone.
    use cistella::error::CistellaError;
    use cistella::isolators::client::rehost_failure_verdict;
    let reported = rehost_failure_verdict(
        Ok(()),
        true,
        CistellaError::Runtime("re-host refused".to_string()),
    )
    .to_string();
    assert_eq!(
        reported, "runtime: re-host refused",
        "original stands, got: {reported}"
    );
}
