//! Runtime: re-export shim over the Podman isolator.
//!
//! The Quadlet lifecycle, shared teardown, and orphan-reaping
//! mechanics moved to [`crate::isolators::podman`] (task 1.2). This
//! module re-exports the unchanged paths so the CLI, tests, and
//! existing callers keep working while the framework trait becomes
//! the primary seam.

pub(crate) use crate::isolators::quadlet::query_unit_props;
pub use crate::isolators::quadlet::{
    GcResult, escape_percent, gc_exited, gc_exited_locked, generate_quadlet_unit, install_quadlet,
    logs_container, quadlet_dir, quote_systemd, remove_scratch, residue_gone,
    residue_gone_for_paths, resolve_image_digest, start_quadlet, systemd_user_available, teardown,
    teardown_inner, unquote_systemd,
};
