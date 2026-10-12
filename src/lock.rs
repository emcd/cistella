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
/// pin behavior against tmpdirs without touching process env.
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
/// The lease file lives in the verified private state directory
/// (`<base>/cistella-state/`, seat-owned 0700) and every write
/// goes through it directory-FD-relative (O_EXCL create,
/// atomic tmp+rename, nofollow opens): a planted symlink can
/// neither clobber a stranger file nor become the record.
///
/// Lifecycle: `create` (O_EXCL, driver pid) before the first
/// prepare spawn, `mark_pending` before each spawn, `record_guest`
/// (clearing pending, appending guest pid and process-group id)
/// once hosted, dropped (best-effort file removal) when guest
/// shutdown is proven and the unit lifecycle converges — or the
/// failure converges with quiescence proven. Drop removes
/// unconditionally: callers keep the handle exactly while
/// quiescence is proven, and `retain` it on paths where shutdown
/// is unproven (residue errors, post-spawn hosting failures) so
/// the uncertainty record outlives the process and stale-ifies
/// naturally for `gc`.
///
/// Content is text lines (`driver <pid>`, optional `pending`,
/// then one `guest <pid> <pgid>` line per hosted guest, hosting
/// order); malformed leases mean skip, never proceed. Session ids
/// are framework-minted (`[a-z0-9-]`); anything else refuses
/// rather than joining a path.
#[derive(Debug)]
pub struct PrepareLease {
    /// Open verified state directory (all file ops relative).
    dir: crate::guest_anchor::AnchoredDir,
    /// Lease file name inside the state directory.
    name: String,
    /// Display path (diagnostics only — never re-resolved).
    path: PathBuf,
    /// Hosted guest identities, in hosting order (all lines are
    /// rewritten on every record — readers never see a torn lease,
    /// and gc skips while ANY recorded group lives).
    guests: Vec<(u32, u32)>,
    /// Spawn recorded but guest identity not yet known: gc skips
    /// (a guest may exist unrecorded — never assume).
    pending: bool,
    /// Uncertainty retained past drop (residue errors, post-spawn
    /// hosting failures): the record must outlive the process and
    /// stale-ify naturally for `gc`.
    retained: bool,
}

impl PrepareLease {
    /// Lease file name for one session id (single-component,
    /// safe for dirfd-relative operations).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on an unmintable session id.
    pub fn file_name(session_id: &str) -> Result<String> {
        if session_id.is_empty()
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(CistellaError::Runtime(format!(
                "prepare lease needs a framework-minted session id: {session_id}"
            )));
        }
        Ok(format!("cistella-prepare-{session_id}"))
    }

    /// Verified private state directory for one seat under
    /// `base`: per-seat namespace (`cistella-state-<uid>`) so one
    /// seat creating the fallback first never locks another seat
    /// uid out of its own state. Created 0700-compatible (the
    /// open enforces ownership and hardens mode).
    pub fn state_dir(base: &std::path::Path) -> PathBuf {
        // SAFETY: getuid is inherently safe.
        let seat = unsafe { nix::libc::getuid() };
        base.join(format!("cistella-state-{seat}"))
    }

    /// Lease file path under `base` for one session id (display
    /// and test compatibility — trust decisions ride the open
    /// directory, never this path).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on an unmintable session id.
    pub fn path_for(base: &std::path::Path, session_id: &str) -> Result<PathBuf> {
        Ok(Self::state_dir(base).join(Self::file_name(session_id)?))
    }

    /// Opens (creating) the verified private state directory.
    fn open_state(base: &std::path::Path) -> Result<crate::guest_anchor::AnchoredDir> {
        crate::guest_anchor::open_state_dir(&Self::state_dir(base))
            .map_err(|error| CistellaError::Runtime(format!("prepare lease state: {error}")))
    }

    /// Renders the full file content from current state.
    fn render(&self) -> Vec<u8> {
        let mut content = format!("driver {}\n", std::process::id());
        if self.pending {
            content.push_str("pending\n");
        }
        for (pid, pgid) in &self.guests {
            content.push_str(&format!("guest {pid} {pgid}\n"));
        }
        content.into_bytes()
    }

    /// Atomically rewrites the record through the held directory.
    fn store(&self) -> Result<()> {
        self.dir
            .replace_file(&self.name, &self.render())
            .map_err(|error| {
                CistellaError::Runtime(format!("prepare lease {}: {error}", self.path.display()))
            })
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
        let name = Self::file_name(session_id)?;
        let path = Self::state_dir(base).join(&name);
        let dir = Self::open_state(base)?;
        let mut file = dir.create_file(&name).map_err(|error| {
            CistellaError::Runtime(format!("prepare lease {}: {error}", path.display()))
        })?;
        use std::io::Write;
        writeln!(file, "driver {}", std::process::id()).map_err(|error| {
            CistellaError::Runtime(format!("prepare lease {}: {error}", path.display()))
        })?;
        Ok(Self {
            dir,
            name,
            path,
            guests: Vec::new(),
            pending: false,
            retained: false,
        })
    }

    /// Marks a spawn as pending BEFORE it happens: gc skips any
    /// lease carrying this marker (a guest may exist unrecorded —
    /// never assume). Cleared by the matching `record_guest`.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on I/O failure.
    pub fn mark_pending(&mut self) -> Result<()> {
        self.pending = true;
        self.store()
    }

    /// Atomically appends one hosted guest pid and process-group id
    /// and clears the pending marker (whole-file tmp+rename —
    /// readers never see a torn lease). Called once per hosted
    /// prepare guest, in hosting order; gc skips while ANY
    /// recorded group lives.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on I/O failure.
    pub fn record_guest(&mut self, pid: u32, pgid: u32) -> Result<()> {
        self.guests.push((pid, pgid));
        self.pending = false;
        self.store()
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
    /// cover the rest. Removal runs inside the held directory
    /// (never by path re-resolution).
    fn drop(&mut self) {
        if !self.retained {
            self.dir.remove_file(&self.name);
        }
    }
}
