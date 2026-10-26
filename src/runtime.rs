//! Runtime: re-export shim over the Podman isolator.
//!
//! The Quadlet lifecycle, shared teardown, and orphan-reaping
//! mechanics moved to [`crate::isolators::podman`]. This
//! module re-exports the unchanged paths so the CLI, tests, and
//! existing callers keep working while the framework trait becomes
//! the primary seam.

pub(crate) use crate::isolators::quadlet::query_unit_props;
pub use crate::isolators::quadlet::{
    escape_percent, generate_quadlet_unit, install_quadlet, logs_container, quadlet_dir,
    quote_systemd, residue_gone, residue_gone_for_paths, resolve_image_digest, start_quadlet,
    systemd_user_available, teardown, teardown_inner, teardown_inner_with_parent, unquote_systemd,
};
pub use crate::isolators::quadlet_gc::{GcResult, gc_exited, gc_exited_locked};

use crate::framework::isolator::{AbsenceObservation, RawClientResult};

/// JSON-encodes one free-text field for the interruption report:
/// quotes included, control bytes/newlines escaped — no terminal
/// injection through observed state strings.
fn report_field(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"<unencodable>\"".to_string())
}

/// Renders the finalized unobservable-outcome report: one stable
/// human line with explicit fields. The container fact renders from
/// the ACTUAL recorded code (`absent` only for exit 1, `present`
/// otherwise — never a fixed claim), service/unit fields render as
/// observed, raw client status renders as `exited(code)` or
/// `signaled(n)` without coercion, and `cleanup` prints `converged`
/// ONLY when the caller proved final absence (the renderer trusts
/// its flag — proof happens at the call site, never here).
#[must_use]
pub fn format_unobservable_report(
    observation: &AbsenceObservation,
    raw: &RawClientResult,
    cleanup_converged: bool,
) -> String {
    let raw_rendered = match raw {
        RawClientResult::Exited(code) => format!("exited({code})"),
        RawClientResult::Signaled(signum) => format!("signaled({signum})"),
    };
    let container_rendered = if observation.container_exists_code == 1 {
        "absent"
    } else {
        // Only 0/1 reach the record (strict constructors and parse
        // bounds); anything else is a defect elsewhere, and presence
        // is the safe reading (never claim absence unproven).
        "present"
    };
    format!(
        "error: execution outcome unobservable (container {} at query, service {}/{} unit {}): harness outcome unobservable; raw client {}; cleanup={}",
        container_rendered,
        report_field(&observation.service_active_state),
        report_field(&observation.service_load_state),
        if observation.unit_file_present {
            "present"
        } else {
            "absent"
        },
        raw_rendered,
        if cleanup_converged {
            "converged"
        } else {
            "unproven"
        },
    )
}
