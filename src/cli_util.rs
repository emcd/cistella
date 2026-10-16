//! CLI selector utilities for the `cistella` binary: label-filter
//! parsing, exact session resolution, directory canonicalization.
//!
//! Bin-crate module (declared in `main.rs`, NOT part of the library):
//! moved verbatim from `main.rs` along verb seams, no logic changes.

use cistella::registry::{list_sessions, resolve_exact};
use cistella::session::parse_cli_label;

/// Parses and validates `--label k=v` filters (refuses `cistella.`).
pub(crate) fn parse_label_filters(
    labels: &[String],
) -> Result<Vec<(String, String)>, cistella::error::CistellaError> {
    labels.iter().map(|a| parse_cli_label(a)).collect()
}

/// Resolves exactly one selector against the live registry.
pub(crate) fn select_exact(
    id: Option<&str>,
    directory: Option<&str>,
    labels: &[String],
) -> Result<cistella::registry::SessionRecord, cistella::error::CistellaError> {
    let records = list_sessions()?;
    let parsed = parse_label_filters(labels)?;
    let canon_dir = directory.map(canonical_directory).transpose()?;
    resolve_exact(&records, id, canon_dir.as_deref(), &parsed)
}

/// Canonicalizes a host directory via longest existing prefix.
///
/// `--session-directory` host defaults to the caller's cwd; both conduct-time and
/// selector-time canonicalize the same way so equality matches.
pub(crate) fn canonical_directory(dir: &str) -> Result<String, cistella::error::CistellaError> {
    if !dir.starts_with('/') {
        return Err(cistella::error::CistellaError::Runtime(format!(
            "directory must be absolute: {dir}"
        )));
    }
    Ok(cistella::mount::canonicalize_host_source(dir)
        .to_string_lossy()
        .to_string())
}
