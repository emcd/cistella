//! Advisory creation-window lock for conduct/terminate/gc.
//!
//! `conduct` holds the lock from before any unit-file or scratch creation
//! through `create unit -> start`; `terminate`/`gc` hold it around
//! scan-and-teardown. A concurrent `gc` therefore never reaps a session
//! that is still being installed.

use std::fs::File;
use std::path::PathBuf;

use nix::fcntl::{Flock, FlockArg};

use crate::error::{CistellaError, Result};

/// Returns the lock file path: `$XDG_RUNTIME_DIR/cistella/lock` with
/// fallback to `/tmp/cistella.lock` when `XDG_RUNTIME_DIR` is absent.
#[must_use]
pub fn lock_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("cistella/lock");
    }
    PathBuf::from("/tmp/cistella.lock")
}

/// Returns the per-session scratch directory for `id`.
///
/// Prefers `$XDG_RUNTIME_DIR/cistella/<id>` (per-user tmpfs with the same
/// lifetime as the user manager); falls back to `/tmp/cistella-<id>`.
#[must_use]
pub fn scratch_dir(session_id: &str) -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join(format!("cistella/{session_id}"));
    }
    PathBuf::from(format!("/tmp/cistella-{session_id}"))
}

/// Returns the legacy `/tmp/cistella-<id>` scratch path.
///
/// Sessions created before the XDG scratch move used this path; teardown
/// removes both paths so orphaned legacy residue never survives.
#[must_use]
pub fn legacy_scratch_dir(session_id: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/cistella-{session_id}"))
}

/// Advisory exclusive lock guard.
///
/// The lock is released when the guard is dropped. Guards must never nest:
/// acquiring a second guard on the same path in the same process deadlocks,
/// so `teardown` offers an inner variant for callers (like `gc`) that
/// already hold the guard.
pub struct LockGuard {
    _locked: Flock<File>,
}

impl LockGuard {
    /// Acquires the lock, blocking until it is available.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` if the lock file cannot be
    /// created or locked.
    pub fn acquire() -> Result<Self> {
        let path = lock_path();
        Self::acquire_path(&path, false)
    }

    /// Acquires the lock without blocking.
    ///
    /// Returns `Ok(None)` when another process holds the lock. Used by
    /// the creation-window test to prove `gc` reaps nothing while
    /// `conduct` is installing.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` if the lock file cannot be
    /// created or an unexpected locking error occurs.
    pub fn try_acquire() -> Result<Option<Self>> {
        let path = lock_path();
        Self::acquire_path(&path, true).map(Some).or_else(|e| {
            if matches!(e, CistellaError::LockContended) {
                Ok(None)
            } else {
                Err(e)
            }
        })
    }

    fn acquire_path(path: &PathBuf, non_blocking: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CistellaError::Runtime(format!("create lock dir {}: {e}", parent.display()))
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| CistellaError::Runtime(format!("open lock {}: {e}", path.display())))?;
        let arg = if non_blocking {
            FlockArg::LockExclusiveNonblock
        } else {
            FlockArg::LockExclusive
        };
        match Flock::lock(file, arg) {
            Ok(locked) => Ok(Self { _locked: locked }),
            Err((_, nix::errno::Errno::EWOULDBLOCK)) if non_blocking => {
                Err(CistellaError::LockContended)
            }
            Err((_, e)) => Err(CistellaError::Runtime(format!(
                "flock {}: {e}",
                path.display()
            ))),
        }
    }
}
