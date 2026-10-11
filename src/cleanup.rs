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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// State-file path for one session id under the runtime base.
/// Session ids are framework-minted; anything else refuses
/// rather than joining a path.
fn state_path(base: &Path, session_id: &str) -> Result<PathBuf> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(CistellaError::Runtime(format!(
            "cleanup state needs a framework-minted session id: {session_id}"
        )));
    }
    Ok(base.join(format!("cistella-cleanup-{session_id}")))
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
    let state = state_path(base, session_id)?;
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
    let bytes = serde_json::to_vec(&records).map_err(|e| {
        CistellaError::Runtime(format!("encode cleanup state {}: {e}", state.display()))
    })?;
    std::fs::write(&state, bytes).map_err(|e| {
        CistellaError::Runtime(format!("write cleanup state {}: {e}", state.display()))
    })?;
    Ok(())
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
    let state = match state_path(base, session_id) {
        Ok(path) => path,
        Err(error) => return vec![error.to_string()],
    };
    let bytes = match std::fs::read(&state) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return litter,
        Err(error) => {
            return vec![format!("cleanup state {}: {error}", state.display())];
        }
    };
    let records: Vec<CleanupRecord> = match serde_json::from_slice(&bytes) {
        Ok(records) => records,
        Err(error) => {
            return vec![format!("cleanup state {}: {error}", state.display())];
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
    // it best-effort.
    if litter.is_empty() {
        let _ = std::fs::remove_file(&state);
    }
    litter
}

/// Admits one recorded root against its identity and removes it
/// FD-relative. Every refusal names the path and becomes litter
/// (never teardown failure).
fn reap_one(record: &CleanupRecord) -> std::result::Result<(), String> {
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let stat = std::fs::symlink_metadata(&record.path)
        .map_err(|e| format!("cleanup {}: {e}", record.path.display()))?;
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
    // Open the parent (nofollow directory), then the root by name
    // within it (nofollow directory): from here no path operation
    // remains — identity was just verified, and every later step
    // is FD-relative.
    let parent_raw = nix::fcntl::open(
        parent,
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| format!("cleanup open parent {}: {e}", parent.display()))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let parent_fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(parent_raw) };
    let Some(name) = record.path.file_name() else {
        return Err(format!(
            "cleanup root names no file: {}",
            record.path.display()
        ));
    };
    let root_raw = nix::fcntl::openat(
        Some(parent_fd.as_raw_fd()),
        Path::new(name),
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| format!("cleanup open root {}: {e}", record.path.display()))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let root_fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(root_raw) };
    // Re-verify identity on the open descriptions (the path stats
    // above and these opens name the same objects only if nothing
    // moved between — the fstat pair closes that window: any swap
    // changes dev/ino and refuses).
    let root_stat = nix::sys::stat::fstat(root_fd.as_raw_fd())
        .map_err(|e| format!("cleanup fstat root {}: {e}", record.path.display()))?;
    if root_stat.st_dev as u64 != record.dev || root_stat.st_ino as u64 != record.ino {
        return Err(format!(
            "cleanup root moved during open (refusing): {}",
            record.path.display()
        ));
    }
    remove_tree(root_fd.as_raw_fd(), 0)
        .map_err(|e| format!("cleanup {}: {e}", record.path.display()))?;
    drop(root_fd);
    nix::unistd::unlinkat(
        Some(parent_fd.as_raw_fd()),
        Path::new(name),
        nix::unistd::UnlinkatFlags::RemoveDir,
    )
    .map_err(|e| format!("cleanup rmdir {}: {e}", record.path.display()))?;
    Ok(())
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
