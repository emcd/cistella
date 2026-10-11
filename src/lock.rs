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

/// Returns the runtime state base: `$XDG_RUNTIME_DIR` when set,
/// else `/tmp`. Session-attributable driver state (prepare
/// leases) lives directly under it, mirroring the lock and
/// scratch fallbacks.
///
/// Callers pass the base explicitly into lease creation so tests
/// pin behavior against tempdirs without touching process env.
#[must_use]
pub fn runtime_base() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    PathBuf::from("/tmp")
}

/// Durable prepare lease: proves a session's prepare phase is (or
/// was recently) hosted, so `gc` never reaps slices of a
/// possibly-live prepare guest.
///
/// Lifecycle: `create` (O_EXCL, driver pid) before the first
/// prepare spawn, `record_guest` (atomic tmp+rename adding guest
/// pid and process-group id) once hosted, dropped (best-effort
/// file removal) when guest shutdown is proven and the unit
/// lifecycle converges — or the failure converges with
/// quiescence proven. Drop removes unconditionally: callers keep
/// the handle exactly while quiescence is proven, and `retain`
/// it on paths where shutdown is unproven (residue errors,
/// hosting failures past spawn) so the uncertainty record
/// outlives the process and stale-ifies naturally for `gc`.
///
/// Content is text lines (`driver <pid>`, then one `guest <pid>
/// <pgid>` line per hosted guest, hosting order); malformed leases
/// mean skip, never proceed. Session ids are framework-minted
/// (`[a-z0-9-]`); anything else refuses rather than joining a path.
#[derive(Debug)]
pub struct PrepareLease {
    /// Lease file (removed on drop unless retained).
    path: PathBuf,
    /// Hosted guest identities, in hosting order (all lines are
    /// rewritten on every record — readers never see a torn lease,
    /// and gc skips while ANY recorded group lives).
    guests: Vec<(u32, u32)>,
    /// Uncertainty retained past drop (residue errors, post-spawn
    /// hosting failures): the record must outlive the process and
    /// stale-ify naturally for `gc`.
    retained: bool,
}

impl PrepareLease {
    /// Lease file path under `base` for one session id.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on an unmintable session id.
    pub fn path_for(base: &std::path::Path, session_id: &str) -> Result<PathBuf> {
        if session_id.is_empty()
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(CistellaError::Runtime(format!(
                "prepare lease needs a framework-minted session id: {session_id}"
            )));
        }
        Ok(base.join(format!("cistella-prepare-{session_id}")))
    }

    /// Creates the lease O_EXCL with the driver pid inside.
    /// EEXIST refuses typed (session ids are unique per conduct —
    /// an existing lease is a stale record or a bug, never
    /// overwritten blindly).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on bad session id,
    /// existing lease, or I/O failure.
    pub fn create(base: &std::path::Path, session_id: &str) -> Result<Self> {
        let path = Self::path_for(base, session_id)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&path).map_err(|e| {
            CistellaError::Runtime(format!("create prepare lease {}: {e}", path.display()))
        })?;
        use std::io::Write;
        writeln!(file, "driver {}", std::process::id()).map_err(|e| {
            CistellaError::Runtime(format!("write prepare lease {}: {e}", path.display()))
        })?;
        Ok(Self {
            path,
            guests: Vec::new(),
            retained: false,
        })
    }

    /// Atomically appends one hosted guest pid and process-group id
    /// (whole-file tmp+rename — readers never see a torn lease).
    /// Called once per hosted prepare guest, in hosting order; gc
    /// skips while ANY recorded group lives.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on I/O failure.
    pub fn record_guest(&mut self, pid: u32, pgid: u32) -> Result<()> {
        self.guests.push((pid, pgid));
        let mut content = format!("driver {}\n", std::process::id());
        for (pid, pgid) in &self.guests {
            content.push_str(&format!("guest {pid} {pgid}\n"));
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, content).map_err(|e| {
            CistellaError::Runtime(format!("write prepare lease {}: {e}", tmp.display()))
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            CistellaError::Runtime(format!("stage prepare lease {}: {e}", self.path.display()))
        })?;
        Ok(())
    }

    /// Retains the lease past drop: shutdown is unproven (residue
    /// errors, post-spawn hosting failures), so the uncertainty
    /// record must outlive the process and stale-ify naturally
    /// for `gc`.
    pub fn retain(&mut self) {
        self.retained = true;
    }
}

impl Drop for PrepareLease {
    /// Best-effort removal unless retained: quiescence was proven
    /// wherever the handle drops unretained; retained records
    /// cover the rest.
    fn drop(&mut self) {
        if !self.retained {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
