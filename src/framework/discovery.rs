//! Sibling-relative guest discovery (task 1.1).
//!
//! External guest binaries ship beside the installed driver
//! executable and are discovered through the current-exe directory
//! only — PATH is never consulted, and names cannot escape the
//! sibling directory (`/` and `..` refuse). A missing or
//! non-executable guest fails conduct pre-create with a typed error
//! naming the expected binary. Discovery returns a path; execution
//! pinning (opened-FD digest) stays in [`super::guest`] at spawn.
//!
//! Trust posture: sibling-relative discovery plus opened-FD digest
//! avoids PATH and replacement races, but does not authenticate a
//! malicious sibling planted before planning. The install directory
//! and both binaries are assumed operator-owned and unmodified.

use std::path::{Path, PathBuf};

use crate::error::{CistellaError, Result};

/// Resolves a guest binary inside `exe_dir`.
///
/// The name is a bare file name: empty names, names containing `/`,
/// and `..` refuse rather than escape the sibling directory. The
/// candidate must exist as a regular file with at least one execute
/// bit; anything else is a typed refusal naming the expected path.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on a bad name, a missing file,
/// or a non-executable candidate. Diagnostics name the path, never
/// anything secret-adjacent (paths here are install layout, not
/// credentials).
pub fn discover_in(exe_dir: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(CistellaError::Contract(format!(
            "guest name must be a bare file name: {name}"
        )));
    }
    let candidate = exe_dir.join(name);
    let metadata = std::fs::symlink_metadata(&candidate).map_err(|_| {
        CistellaError::Contract(format!(
            "guest binary absent from install directory: {}",
            candidate.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(CistellaError::Contract(format!(
            "guest binary is not a regular file: {}",
            candidate.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(CistellaError::Contract(format!(
                "guest binary is not executable: {}",
                candidate.display()
            )));
        }
    }
    Ok(candidate)
}

/// Resolves a guest binary beside the running driver executable.
///
/// # Errors
///
/// Returns `CistellaError::Contract` when the current executable
/// path is unavailable or [`discover_in`] refuses.
pub fn discover_sibling(name: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| {
        CistellaError::Contract(format!("current executable path unavailable: {e}"))
    })?;
    let dir = exe.parent().ok_or_else(|| {
        CistellaError::Contract("current executable has no parent directory".to_string())
    })?;
    discover_in(dir, name)
}
