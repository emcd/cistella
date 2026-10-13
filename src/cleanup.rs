//! Extension-contributed host-state cleanup: reap declared roots.
//!
//! The framework removes ONLY explicitly declared cleanup paths —
//! never every contributed mount. Each declared root is admitted
//! at reap time against the identity recorded when the plan was
//! admitted (dev+ino of the root plus its parent association);
//! traversal runs via anchored directory FDs with ordinary files
//! unlinked unfollowed and nested symlinks unlinked as entries
//! (never traversed). Removal failure is litter, never residue:
//! reap reports per-entry litter strings and never fails teardown.
//!
//! Declarations reach teardown through a per-session state file
//! (JSON list written post-prepare); its absence means nothing
//! declared. Orphaned slices without plans reconcile through `gc`
//! by session-attributable enumeration, never through this module.

use std::ffi::OsStr;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CistellaError, Result};

/// Recursion ceiling for FD-relative traversal (fail-closed past
/// it — session slices are shallow; depth beyond this is either an
/// attack or a bug, both litter rather than traversal risk).
const MAX_TRAVERSAL_DEPTH: u32 = 64;

/// Admitted cleanup root: the declared path plus the identity
/// observed when the plan was admitted (the only point where the
/// object is known good). Ownership plus kind alone cannot
/// distinguish a replacement directory from the minted slice;
/// the recorded identity can.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupRecord {
    /// Declared absolute host path.
    pub path: PathBuf,
    /// Root device at admission.
    pub dev: u64,
    /// Root inode at admission.
    pub ino: u64,
    /// Parent device at admission (association).
    pub parent_dev: u64,
    /// Parent inode at admission (association).
    pub parent_ino: u64,
}

/// Admits declared cleanup paths and records the state file:
/// every path must be absolute and name an existing non-symlink
/// directory owned by the seat or root (observed now, identity
/// recorded for reap). Writes nothing when the list is empty
/// (absent state means nothing declared).
///
/// Call after prepare succeeds, before staging or create: the
/// guest is shut down (proven), so observation is stable against
/// guest races — only seat/root could alter objects past here,
/// per the ownership gate.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on bad session id, relative
/// paths, symlink/non-directory/untrusted roots, or I/O failure.
pub fn record_cleanup(base: &Path, session_id: &str, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    // Validate the session id before guest-supplied paths:
    // programmer errors surface before untrusted-input errors.
    let _session = cleanup_state_name(session_id)
        .map_err(|error| CistellaError::Runtime(format!("cleanup state: {error}")))?;
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        let candidate = PathBuf::from(path);
        if !candidate.is_absolute() {
            return Err(CistellaError::Runtime(format!(
                "cleanup declaration must be absolute: {path}"
            )));
        }
        let stat = std::fs::symlink_metadata(&candidate).map_err(|e| {
            CistellaError::Runtime(format!("cleanup admit {}: {e}", candidate.display()))
        })?;
        if stat.file_type().is_symlink() {
            return Err(CistellaError::Runtime(format!(
                "cleanup root must not be a symlink: {}",
                candidate.display()
            )));
        }
        if !stat.is_dir() {
            return Err(CistellaError::Runtime(format!(
                "cleanup root must be a directory: {}",
                candidate.display()
            )));
        }
        if stat.uid() != seat && stat.uid() != 0 {
            return Err(CistellaError::Runtime(format!(
                "cleanup root must be seat- or root-owned: {}",
                candidate.display()
            )));
        }
        let parent = candidate.parent().ok_or_else(|| {
            CistellaError::Runtime(format!(
                "cleanup root has no parent: {}",
                candidate.display()
            ))
        })?;
        let parent_stat = std::fs::metadata(parent).map_err(|e| {
            CistellaError::Runtime(format!("cleanup admit {}: {e}", parent.display()))
        })?;
        records.push(CleanupRecord {
            path: candidate,
            dev: stat.dev(),
            ino: stat.ino(),
            parent_dev: parent_stat.dev(),
            parent_ino: parent_stat.ino(),
        });
    }
    let bytes = serde_json::to_vec(&records)
        .map_err(|e| CistellaError::Runtime(format!("encode cleanup state: {e}")))?;
    let name = cleanup_state_name(session_id)
        .map_err(|error| CistellaError::Runtime(format!("cleanup state: {error}")))?;
    open_session_state(base)
        .map_err(|error| CistellaError::Runtime(format!("cleanup state: {error}")))?
        .replace_file(&name, &bytes)
        .map_err(|e| CistellaError::Runtime(format!("write cleanup state {name}: {e}")))?;
    Ok(())
}

/// Opens the verified private session-state directory under
/// `base` (created 0700 seat-owned on first use).
fn open_session_state(
    base: &Path,
) -> std::result::Result<crate::guest_anchor::AnchoredDir, String> {
    crate::guest_anchor::open_state_dir(&crate::lock::PrepareLease::state_dir(base))
}

/// Reaps the recorded cleanup roots for one session: admits each
/// root against its recorded identity, removes FD-relative, and
/// returns per-entry litter strings (never fails — removal
/// failure is litter, never residue). Absent state file means
/// nothing declared (empty litter). The state file is removed
/// best-effort after a fully clean reap; refused entries keep
/// their record for retry.
pub fn reap_cleanup(base: &Path, session_id: &str) -> Vec<String> {
    let mut litter = Vec::new();
    let name = match cleanup_state_name(session_id) {
        Ok(name) => name,
        Err(error) => return vec![error.to_string()],
    };
    let dir = match open_session_state(base) {
        Ok(dir) => dir,
        Err(error) => return vec![format!("cleanup state: {error}")],
    };
    let bytes = match dir.read_file(&name, crate::guest_anchor::MAX_STATE_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return litter,
        Err(error) => return vec![format!("cleanup state {name}: {error}")],
    };
    let records: Vec<CleanupRecord> = match serde_json::from_slice(&bytes) {
        Ok(records) => records,
        Err(error) => {
            return vec![format!("cleanup state {name}: {error}")];
        }
    };
    for record in &records {
        if let Err(error) = reap_one(record) {
            litter.push(error);
        }
    }
    // The state file survives litter: refused entries stay
    // retryable by the next teardown or gc (dropping the record
    // on refusal would orphan them silently). Clean reaps remove
    // it best-effort inside the held directory.
    if litter.is_empty() {
        dir.remove_file(&name);
    }
    litter
}

/// Reaps declared cleanup roots, reporting refusals as warnings.
/// Shared by teardown paths that own no unit lifecycle of their
/// own: the podman-native `terminate` command (the quadlet path
/// covers this in `teardown_inner`; orderly terminate left
/// declared slices behind without it). Litter never fails the
/// caller (the unit is gone either way — removal failure is
/// litter, never residue).
pub fn reap_and_report(base: &Path, session_id: &str) {
    for litter in reap_cleanup(base, session_id) {
        eprintln!("warning: cleanup litter: {litter}");
    }
}

/// Cleanup state file name for one session id (single-component,
/// safe for dirfd-relative operations).
fn cleanup_state_name(session_id: &str) -> std::result::Result<String, String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(format!(
            "cleanup state needs a framework-minted session id: {session_id}"
        ));
    }
    Ok(format!("cistella-cleanup-{session_id}"))
}

/// Admits one recorded root against its identity and removes it
/// FD-relative. Every refusal names the path and becomes litter
/// (never teardown failure). An already-gone root is idempotently
/// complete (converged by other means), not litter.
fn reap_one(record: &CleanupRecord) -> std::result::Result<(), String> {
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let stat = match std::fs::symlink_metadata(&record.path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!("cleanup {}: {error}", record.path.display()));
        }
    };
    if stat.file_type().is_symlink() {
        return Err(format!(
            "cleanup root is a symlink (refusing swap): {}",
            record.path.display()
        ));
    }
    if !stat.is_dir() {
        return Err(format!(
            "cleanup root is not a directory: {}",
            record.path.display()
        ));
    }
    if stat.uid() != seat && stat.uid() != 0 {
        return Err(format!(
            "cleanup root is not seat-owned: {}",
            record.path.display()
        ));
    }
    if stat.dev() != record.dev || stat.ino() != record.ino {
        return Err(format!(
            "cleanup root identity changed (refusing replacement): {}",
            record.path.display()
        ));
    }
    let parent = record
        .path
        .parent()
        .ok_or_else(|| format!("cleanup root has no parent: {}", record.path.display()))?;
    let parent_stat = std::fs::metadata(parent)
        .map_err(|e| format!("cleanup parent {}: {e}", parent.display()))?;
    if parent_stat.dev() != record.parent_dev || parent_stat.ino() != record.parent_ino {
        return Err(format!(
            "cleanup parent changed (refusing move): {}",
            record.path.display()
        ));
    }
    // Open the trusted parent, re-verify the association on
    // the open description, then remove through the shared
    // record-carried tail.
    let parent_fd = open_trusted_parent(parent, &record.path.display().to_string())?;
    let parent_open = nix::sys::stat::fstat(parent_fd.as_raw_fd())
        .map_err(|e| format!("cleanup fstat parent {}: {e}", parent.display()))?;
    if parent_open.st_dev as u64 != record.parent_dev
        || parent_open.st_ino as u64 != record.parent_ino
    {
        return Err(format!(
            "cleanup parent moved during open (refusing): {}",
            record.path.display()
        ));
    }
    let Some(name) = record.path.file_name() else {
        return Err(format!(
            "cleanup root names no file: {}",
            record.path.display()
        ));
    };
    unlink_verified_tree(
        &parent_fd,
        name,
        Some((record.dev, record.ino)),
        &record.path.display().to_string(),
    )
}

/// Opens a parent directory nofollow and enforces traversal
/// trust on the open description (owner seat/root, entry
/// mutation restricted to trusted actors). Shared by declared
/// and enumerated removals — every mutating path operation in
/// this module descends from a directory opened here.
fn open_trusted_parent(parent: &Path, display: &str) -> std::result::Result<OwnedFd, String> {
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let parent_raw = nix::fcntl::open(
        parent,
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| format!("cleanup open parent {display}: {e}"))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let parent_fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(parent_raw) };
    let parent_open = nix::sys::stat::fstat(parent_fd.as_raw_fd())
        .map_err(|e| format!("cleanup fstat parent {display}: {e}"))?;
    traversal_trusted(&parent_open, seat, &format!("parent {display}"))?;
    Ok(parent_fd)
}

/// Removes one verified object through its trusted parent FD:
/// opens the name nofollow, verifies directory kind, seat
/// ownership, and dev+ino identity on the open description
/// (against the admitted baseline, or the fresh open when no
/// baseline exists), traverses FD-relative, re-verifies the
/// name before unlink, and unlinks from the parent FD. Both
/// declared-path reap (baseline from the record) and
/// gc-enumerated removal (baseline from first open) share this
/// tail, so no deletion runs on a bare bool — the record (or
/// the fresh open it stands in for) travels into the actual
/// deletion.
fn unlink_verified_tree(
    parent_fd: &OwnedFd,
    name: &std::ffi::OsStr,
    baseline: Option<(u64, u64)>,
    display: &str,
) -> std::result::Result<(), String> {
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let root_raw = nix::fcntl::openat(
        Some(parent_fd.as_raw_fd()),
        Path::new(name),
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| format!("cleanup open root {display}: {e}"))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let root_fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(root_raw) };
    let root_stat = nix::sys::stat::fstat(root_fd.as_raw_fd())
        .map_err(|e| format!("cleanup fstat root {display}: {e}"))?;
    if root_stat.st_uid != seat && root_stat.st_uid != 0 {
        return Err(format!("cleanup root is not seat-owned: {display}"));
    }
    let (dev, ino) = baseline.unwrap_or((root_stat.st_dev as u64, root_stat.st_ino as u64));
    if root_stat.st_dev as u64 != dev || root_stat.st_ino as u64 != ino {
        return Err(format!(
            "cleanup root moved during open (refusing): {display}"
        ));
    }
    remove_tree(root_fd.as_raw_fd(), 0).map_err(|e| format!("cleanup {display}: {e}"))?;
    drop(root_fd);
    // Re-verify the name still denotes the traversed object
    // before unlinking: a directory-entry replacement after
    // traversal would otherwise delete the wrong directory.
    // Best-effort under trusted writers — claims no atomicity
    // even for them.
    let recheck_raw = nix::fcntl::openat(
        Some(parent_fd.as_raw_fd()),
        Path::new(name),
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| format!("cleanup reverify {display}: {e}"))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let recheck_fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(recheck_raw) };
    let recheck = nix::sys::stat::fstat(recheck_fd.as_raw_fd())
        .map_err(|e| format!("cleanup fstat {display}: {e}"))?;
    drop(recheck_fd);
    if recheck.st_dev as u64 != dev || recheck.st_ino as u64 != ino {
        return Err(format!(
            "cleanup root rebound during traversal (refusing): {display}"
        ));
    }
    nix::unistd::unlinkat(
        Some(parent_fd.as_raw_fd()),
        Path::new(name),
        nix::unistd::UnlinkatFlags::RemoveDir,
    )
    .map_err(|e| format!("cleanup rmdir {display}: {e}"))?;
    Ok(())
}

/// Traversal trust for one open directory description: the
/// owner must be the seat or root, and entry mutation
/// (rename/remove/bind) must be restricted to trusted actors —
/// no group/other write bits at all, or sticky with a trusted
/// owner (group members cannot rename foreign entries under
/// sticky). Rationale: deletion paths unlink by name, so any
/// actor able to rename entries in a traversed directory could
/// swap the final unlink target. This is STRICTER than the shared
/// mount-ancestor rule on purpose (deletion needs the stronger
/// guarantee — ancestor_verdict tolerates group-write, which
/// admits untrusted renamers here). Every directory the
/// traversal descends through or unlinks from meets this on its
/// OPEN FD, so the check-then-use window admits only trusted
/// interleavers — stated explicitly as the trusted-writer scope
/// for all mutating path operations in this module. Name
/// re-verification before each unlink narrows even trusted-actor
/// races further, but claims no atomicity even for them.
fn traversal_trusted(
    stat: &nix::sys::stat::FileStat,
    seat: nix::libc::uid_t,
    what: &str,
) -> std::result::Result<(), String> {
    if stat.st_uid != seat && stat.st_uid != 0 {
        return Err(format!("cleanup traversal owner untrusted: {what}"));
    }
    let mode = stat.st_mode;
    if mode & 0o022 == 0 {
        return Ok(());
    }
    if mode & 0o1000 != 0 {
        return Ok(());
    }
    if mode & 0o002 != 0 {
        return Err(format!("cleanup traversal parent world-writable: {what}"));
    }
    Err(format!("cleanup traversal parent group-writable: {what}"))
}

/// Removes a directory's CONTENTS via its open FD (the directory
/// itself is removed by the caller): subdirectories recurse by
/// FD, ordinary files and symlinks unlink as entries without
/// following, anything else unlinks as a name. Depth-capped,
/// fail-closed.
fn remove_tree(dirfd: RawFd, depth: u32) -> std::result::Result<(), String> {
    if depth > MAX_TRAVERSAL_DEPTH {
        return Err("cleanup traversal too deep".to_string());
    }
    // Every descended directory meets traversal trust on its OPEN
    // FD (owner + mutation-restricted mode): untrusted writers
    // anywhere on the descent path refuse the whole removal
    // rather than risk operating among attacker-controlled entries.
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let here = nix::sys::stat::fstat(dirfd).map_err(|e| format!("cleanup fstat: {e}"))?;
    traversal_trusted(&here, seat, "traversed directory")?;
    let dup = nix::unistd::dup(dirfd).map_err(|e| format!("cleanup dup: {e}"))?;
    // SAFETY: freshly duped above, owned here, wrapped exactly once.
    let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(dup) };
    let mut dir =
        nix::dir::Dir::from_fd(owned.into_raw_fd()).map_err(|e| format!("cleanup opendir: {e}"))?;
    let entries: Vec<(std::ffi::CString, Option<nix::dir::Type>)> = dir
        .iter()
        .map(|result| {
            result
                .map(|entry| (entry.file_name().to_owned(), entry.file_type()))
                .map_err(|e| format!("cleanup readdir: {e}"))
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    for (name, file_type) in entries {
        // Dir iteration yields `.` and `..` — never operate on them.
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        let name_path = Path::new(OsStr::from_bytes(name.as_bytes()));
        match file_type {
            Some(nix::dir::Type::Directory) => {
                let sub = nix::fcntl::openat(
                    Some(dirfd),
                    name_path,
                    nix::fcntl::OFlag::O_RDONLY
                        | nix::fcntl::OFlag::O_NOFOLLOW
                        | nix::fcntl::OFlag::O_DIRECTORY
                        | nix::fcntl::OFlag::O_CLOEXEC,
                    nix::sys::stat::Mode::empty(),
                )
                .map_err(|e| format!("cleanup open subdir {}: {e}", name.to_string_lossy()))?;
                remove_tree(sub, depth + 1).inspect_err(|_| {
                    let _ = nix::unistd::close(sub);
                })?;
                nix::unistd::close(sub).map_err(|e| format!("cleanup close: {e}"))?;
                nix::unistd::unlinkat(
                    Some(dirfd),
                    name_path,
                    nix::unistd::UnlinkatFlags::RemoveDir,
                )
                .map_err(|e| format!("cleanup rmdir {}: {e}", name.to_string_lossy()))?;
            }
            _ => {
                // Files, symlinks, and anything else unlink as
                // entries: removing a link never follows it, and
                // DT_UNKNOWN falls here too (the unlink needs no
                // type — only directories take the branch above).
                nix::unistd::unlinkat(
                    Some(dirfd),
                    name_path,
                    nix::unistd::UnlinkatFlags::NoRemoveDir,
                )
                .map_err(|e| format!("cleanup unlink {}: {e}", name.to_string_lossy()))?;
            }
        }
    }
    Ok(())
}

/// Seat temp root for slice mint and orphan enumeration: host
/// `/tmp`, unconditionally (no environment selection, no legacy
/// roots — pre-1.0 breaks freely). Mint and gc share this one
/// root by construction.
#[must_use]
pub fn seat_temp_root() -> PathBuf {
    PathBuf::from("/tmp")
}

/// Parsed prepare-lease state: driver pid, pending marker, plus
/// one guest pid and process-group line per hosted guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseState {
    /// Driver pid at lease creation.
    pub driver_pid: u32,
    /// A spawn is recorded but its guest identity is not yet
    /// known: gc skips (a guest may exist unrecorded).
    pub pending: bool,
    /// Hosted guests (pid, process-group id), hosting order.
    pub guests: Vec<(u32, u32)>,
}

/// Reads and parses one prepare lease: `None` when absent (no
/// prepare in flight and none recorded); `Err` on malformed
/// content or I/O failure (malformed means skip, never proceed).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on unreadable or malformed
/// lease content.
pub fn read_lease(base: &Path, session_id: &str) -> Result<Option<LeaseState>> {
    let name = crate::lock::PrepareLease::file_name(session_id)
        .map_err(|error| CistellaError::Runtime(format!("prepare lease: {error}")))?;
    let dir = open_session_state(base)
        .map_err(|error| CistellaError::Runtime(format!("prepare lease state: {error}")))?;
    let bytes = match dir.read_file(&name, crate::guest_anchor::MAX_STATE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(CistellaError::Runtime(format!(
                "prepare lease {name}: {error}"
            )));
        }
    };
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| CistellaError::Runtime(format!("prepare lease {name} is not valid UTF-8")))?;
    let mut lines = text.lines().peekable();
    let first = lines.next().unwrap_or("");
    let mut words = first.split_whitespace();
    let driver_pid: u32 = match words.next() {
        Some("driver") => words
            .next()
            .and_then(|pid| pid.parse().ok())
            .ok_or_else(|| {
                CistellaError::Runtime(format!("prepare lease {name} has a malformed driver line"))
            })?,
        _ => {
            return Err(CistellaError::Runtime(format!(
                "prepare lease {name} has a malformed driver line"
            )));
        }
    };
    let mut pending = false;
    let mut guests = Vec::new();
    for line in lines {
        let mut words = line.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some("pending"), None, None) => pending = true,
            (Some("guest"), Some(pid), Some(pgid)) => {
                let pid: u32 = pid.parse().map_err(|_| {
                    CistellaError::Runtime(format!(
                        "prepare lease {name} has a malformed guest line"
                    ))
                })?;
                let pgid: u32 = pgid.parse().map_err(|_| {
                    CistellaError::Runtime(format!(
                        "prepare lease {name} has a malformed guest line"
                    ))
                })?;
                guests.push((pid, pgid));
            }
            _ => {
                return Err(CistellaError::Runtime(format!(
                    "prepare lease {name} has a malformed line"
                )));
            }
        }
    }
    Ok(Some(LeaseState {
        driver_pid,
        pending,
        guests,
    }))
}

/// True when a pid is observably alive (signal-0 succeeds or is
/// refused for ownership — only ESRCH proves absence; every other
/// outcome keeps uncertainty, i.e. alive). Pid 0 never names a
/// reapable process (it addresses the caller's own group) and
/// reads as alive.
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return true;
    }
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::ESRCH) => false,
        Err(_) => true,
    }
}

/// True only when a process group is proven absent (ESRCH): the
/// sole establishment accepted for quiescence. Any other outcome
/// — including signal errors — keeps uncertainty (not dead).
/// Never signals: observation only, so reused PIDs cannot be
/// killed through this check. Pgid 0 reads as alive (it would
/// address the caller's own group).
#[must_use]
pub fn group_dead(pgid: u32) -> bool {
    if pgid == 0 {
        return false;
    }
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(-(pgid as i32)), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

/// Parses a session id out of a slice directory name
/// (`cistella-tmp-<session>-<pid>-<nanos>`): the trailing two
/// numeric segments are the mint randomness, everything between
/// the prefix and them is the session. Anything else (wrong
/// prefix, missing numerics, bad session charset) is not ours —
/// `None` means skip silently, never touch.
#[must_use]
pub fn parse_slice_session(name: &str) -> Option<String> {
    let rest = name.strip_prefix("cistella-tmp-")?;
    let mut parts: Vec<&str> = rest.split('-').collect();
    if parts.len() < 3 {
        return None;
    }
    let nanos = parts.pop().expect("len checked");
    let pid = parts.pop().expect("len checked");
    if nanos.is_empty()
        || pid.is_empty()
        || !nanos.bytes().all(|byte| byte.is_ascii_digit())
        || !pid.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let session = parts.join("-");
    if session.is_empty()
        || !session
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    Some(session)
}

/// Reaps orphaned tmpdir slices: enumerates the seat temp root
/// for session-attributable names, skips sessions with live units
/// and anything not provably quiescent, and removes the rest
/// FD-relative. Returns removed directory names; litter reports
/// to stderr (removal failure is litter, never residue) and the
/// caller continues.
///
/// Safety conditions per candidate, all required: name parses to
/// a session absent from `live_sessions`; the prepare lease is
/// absent (no prepare recorded) or present with a dead driver,
/// all recorded guest groups proven dead, and at least one guest
/// line (guestless leases skip — contradictory provenance);
/// any live driver, live group, unreadable/malformed lease, or
/// identity gap skips. Removal itself opens temproot and candidate
/// nofollow, verifies directory plus seat-or-root ownership on
/// the open descriptions, traverses FD-relative, and unlinks —
/// orphans carry no recorded identity (nothing was admitted), so
/// name-attribution plus liveness gates are the safety, never a
/// baseline comparison.
pub fn reap_tmpdir_orphans(live_sessions: &std::collections::HashSet<String>) -> Vec<String> {
    reap_tmpdir_orphans_in(
        &seat_temp_root(),
        &crate::lock::runtime_base(),
        live_sessions,
    )
}

/// Orphan sweep with explicit roots (the unit-pinned entry point):
/// `root` enumerates candidate slices, `base` holds lease state.
/// Same safety contract as [`reap_tmpdir_orphans`].
pub fn reap_tmpdir_orphans_in(
    root: &Path,
    base: &Path,
    live_sessions: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut removed = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return removed,
        Err(error) => {
            eprintln!("warning: cleanup enumerate {}: {error}", root.display());
            return removed;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(session) = parse_slice_session(&name) else {
            continue;
        };
        if live_sessions.contains(&session) {
            continue;
        }
        match orphan_eligible(base, &session) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(litter) => {
                eprintln!("warning: cleanup litter: {litter}");
                continue;
            }
        }
        // Record rendezvous: a session WITH cleanup records
        // never falls back to name-only authority. An admitted
        // identity removes through the shared record-carried
        // tail; a recorded path with a mismatched object skips
        // (a teardown-refused replacement stands); genuinely
        // unrecorded paths take the name-attribution path.
        let candidate = root.join(&name);
        match match_cleanup_record(base, &session, &candidate) {
            Ok(RecordMatch::Admitted(record)) => {
                remove_lease_file(base, &session);
                match reap_recorded(&candidate, &record) {
                    Ok(()) => removed.push(name),
                    Err(litter) => eprintln!("warning: cleanup litter: {litter}"),
                }
            }
            Ok(RecordMatch::Refused) => {
                eprintln!(
                    "warning: cleanup litter: recorded refusal stands for {}",
                    candidate.display()
                );
                continue;
            }
            Ok(RecordMatch::Unrecorded) => {
                remove_lease_file(base, &session);
                match remove_orphan(root, &name) {
                    Ok(()) => removed.push(name),
                    Err(litter) => eprintln!("warning: cleanup litter: {litter}"),
                }
            }
            Ok(RecordMatch::Gone) => continue,
            Err(litter) => {
                eprintln!("warning: cleanup litter: {litter}");
                continue;
            }
        }
    }
    removed
}

/// Removes one stale lease file best-effort (never fails —
/// callers treat removal as hygiene, eligibility as safety).
fn remove_lease_file(base: &Path, session: &str) {
    let Ok(name) = crate::lock::PrepareLease::file_name(session) else {
        return;
    };
    if let Ok(dir) = open_session_state(base) {
        dir.remove_file(&name);
    }
}

/// Decides one candidate: true when no live unit, no live lease,
/// no pending spawn, and no live recorded guest group stand in
/// the way. `Err` is litter (malformed lease, unreadable state)
/// and skips.
fn orphan_eligible(base: &Path, session: &str) -> std::result::Result<bool, String> {
    let state = match read_lease(base, session) {
        Ok(state) => state,
        Err(error) => return Err(error.to_string()),
    };
    let Some(state) = state else {
        return Ok(true);
    };
    if pid_alive(state.driver_pid) {
        return Ok(false);
    }
    if state.pending {
        // A spawn is recorded but its guest identity is not yet
        // known: a guest may exist unrecorded (spawn precedes
        // recording by microseconds) — never assume.
        return Ok(false);
    }
    if state.guests.is_empty() {
        // No guest lines, no pending marker, dead driver: spawn
        // never happened (pending always commits first), so no
        // guest exists — proceed; the stale lease file goes with
        // the orphan.
        return Ok(true);
    }
    for (_, pgid) in &state.guests {
        if !group_dead(*pgid) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Record rendezvous for one orphan candidate: loads the
/// session's cleanup records (absent file means unrecorded) and
/// matches by object identity, never by name alone:
/// - `Admitted` — some record's dev+ino names this object: the
///   admitted slice, wherever it currently sits.
/// - `Refused` — a record names this path but nothing matches its
///   identity: a teardown-refused replacement stands.
/// - `Unrecorded` — no records, or none relevant: the existing
///   name-attribution path may proceed.
/// - `Gone` — the candidate vanished between enumeration and
///   lookup: converged by other means, skip silently.
///
/// Unreadable state is litter (uncertainty skips).
fn match_cleanup_record(
    base: &Path,
    session: &str,
    candidate: &Path,
) -> std::result::Result<RecordMatch, String> {
    let dir = match open_session_state(base) {
        Ok(dir) => dir,
        Err(error) => return Err(format!("cleanup state: {error}")),
    };
    let name = match cleanup_state_name(session) {
        Ok(name) => name,
        Err(error) => return Err(format!("cleanup state: {error}")),
    };
    let bytes = match dir.read_file(&name, crate::guest_anchor::MAX_STATE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => return Err(format!("cleanup state {name}: {error}")),
    };
    let Some(bytes) = bytes else {
        return Ok(RecordMatch::Unrecorded);
    };
    let records: Vec<CleanupRecord> = match serde_json::from_slice(&bytes) {
        Ok(records) => records,
        Err(error) => return Err(format!("cleanup state {name}: {error}")),
    };
    let stat = match std::fs::symlink_metadata(candidate) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(RecordMatch::Gone),
        Err(error) => return Err(format!("cleanup {error} {}", candidate.display())),
    };
    // Full match first: identity plus the admitted parent
    // association. A dev/ino hit without association (moved
    // object) does NOT admit — it falls through to the path
    // check, which refuses when a record names this path.
    // Checking every record before deciding keeps multiple
    // records per session coherent.
    let mut identity_seen = false;
    for record in &records {
        if stat.dev() == record.dev && stat.ino() == record.ino {
            identity_seen = true;
            let associated = candidate
                .parent()
                .and_then(|parent| std::fs::metadata(parent).ok())
                .is_some_and(|parent_stat| {
                    parent_stat.dev() == record.parent_dev && parent_stat.ino() == record.parent_ino
                });
            if associated {
                return Ok(RecordMatch::Admitted(record.clone()));
            }
        }
    }
    if identity_seen || records.iter().any(|record| record.path == *candidate) {
        // Either the admitted object moved away from here, or a
        // record names this path for a different object: both are
        // teardown-refused states — preserve.
        return Ok(RecordMatch::Refused);
    }
    Ok(RecordMatch::Unrecorded)
}
/// Record-match outcome for one gc candidate (see
/// [`match_cleanup_record`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordMatch {
    /// A record's dev+ino names this object: admitted removal.
    Admitted(CleanupRecord),
    /// A record names this path but nothing matches: a refused
    /// replacement stands — preserve it.
    Refused,
    /// No records relevant: name-attribution path may proceed.
    Unrecorded,
    /// Candidate vanished: converged by other means.
    Gone,
}

/// Removes one admitted object found by enumeration: the match
/// already verified identity plus the admitted parent
/// association, so the parent here is trusted on its open FD and
/// the record travels into the shared verified-removal tail.
/// Used only after a full match, never on a bare name.
fn reap_recorded(candidate: &Path, record: &CleanupRecord) -> std::result::Result<(), String> {
    let parent = candidate.parent().ok_or_else(|| {
        format!(
            "cleanup admitted root has no parent: {}",
            candidate.display()
        )
    })?;
    let parent_fd = open_trusted_parent(parent, &candidate.display().to_string())?;
    // Association on the ACTUAL opened parent FD (not just the
    // path-metadata check at match time): the parent could have
    // been swapped between the match lookup and this open, so
    // the full record stays effective through deletion.
    let parent_open = nix::sys::stat::fstat(parent_fd.as_raw_fd())
        .map_err(|e| format!("cleanup fstat parent {}: {e}", parent.display()))?;
    if parent_open.st_dev as u64 != record.parent_dev
        || parent_open.st_ino as u64 != record.parent_ino
    {
        return Err(format!(
            "cleanup parent moved during open (refusing): {}",
            candidate.display()
        ));
    }
    let Some(name) = candidate.file_name() else {
        return Err(format!(
            "cleanup admitted root names no file: {}",
            candidate.display()
        ));
    };
    unlink_verified_tree(
        &parent_fd,
        name,
        Some((record.dev, record.ino)),
        &candidate.display().to_string(),
    )
}

/// Removes one enumerated orphan with no record: the temproot
/// meets traversal trust on its open FD, then the shared tail
/// verifies kind plus ownership on the open description and
/// removes FD-relative with a pre-unlink reverify (baseline from
/// the fresh open — unrecorded objects carry no admitted
/// identity, so name-attribution plus liveness gates decided
/// eligibility before this call).
fn remove_orphan(root: &Path, name: &str) -> std::result::Result<(), String> {
    let parent_fd = open_trusted_parent(root, name)?;
    unlink_verified_tree(&parent_fd, Path::new(name).as_os_str(), None, name)
}
