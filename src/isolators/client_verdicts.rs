use crate::error::{CistellaError, Result};

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
