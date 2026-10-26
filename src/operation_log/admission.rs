//! Admission control: the nonblocking lock, owner-attributed
//! capacity reservations, stale-entry reaping, and the tree usage
//! scan. Everything here fails closed toward *less* logging, never
//! toward a changed business decision.

use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::path::Path;

use super::{OP_METADATA_SHARE, PER_OP_CAP};
use super::{Owner, open_verified_dir, owner_alive, self_owner};
use crate::error::{CistellaError, Result};

/// Capacity reserved per open operation: metadata share plus one
/// full per-operation cap. Tests pin admission at this exact
/// quantum.
pub const RESERVATION_BYTES: u64 = OP_METADATA_SHARE + PER_OP_CAP;

/// Name of the admission lock file under the log root.
const ADMISSION_LOCK: &str = ".admission.lock";

/// Directory holding one reservation file per open operation.
const RESERVED_DIR: &str = ".reserved";

/// Acquires the admission lock without blocking: the critical
/// section is a bounded scan plus tiny reads and writes, and a
/// blocking wait here could stall the business operation.
/// Contention skips logging (observability loss only, never a
/// business decision).
/// Acquires the admission lock, retrying briefly: finalization
/// holds it across the terminal write plus fsync, so a few
/// retries ride out a concurrent prune or open. The total wait is
/// bounded (tries times 100 microseconds); exhaustion falls back
/// to unprotected finalization rather than stalling the business
/// operation.
pub(crate) fn admission_lock_bounded(
    root: &crate::guest_anchor::AnchoredDir,
    tries: u32,
) -> Result<nix::fcntl::Flock<std::fs::File>> {
    for _ in 0..tries {
        match admission_lock(root) {
            Ok(guard) => return Ok(guard),
            Err(_) => std::thread::sleep(std::time::Duration::from_micros(100)),
        }
    }
    admission_lock(root)
}

pub(crate) fn admission_lock(
    root: &crate::guest_anchor::AnchoredDir,
) -> Result<nix::fcntl::Flock<std::fs::File>> {
    use nix::fcntl::{Flock, FlockArg};
    let raw = nix::fcntl::openat(
        Some(root.dirfd()),
        std::path::Path::new(ADMISSION_LOCK),
        nix::fcntl::OFlag::O_RDWR
            | nix::fcntl::OFlag::O_CREAT
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .map_err(|errno| CistellaError::Runtime(format!("operation log admission: {errno}")))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let file: std::fs::File = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw).into() };
    // A planted non-regular file (symlink target aside, a FIFO
    // would make a blocking opener hang elsewhere): the lock must
    // be a regular file or there is no lock to hold.
    {
        if !file
            .metadata()
            .map(|meta| meta.file_type().is_file() && !meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(CistellaError::Runtime(
                "operation log admission is not a regular file".to_string(),
            ));
        }
    }
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(locked) => Ok(locked),
        Err((_, errno)) => Err(CistellaError::Runtime(format!(
            "operation log admission locked: {errno}"
        ))),
    }
}

/// Opens (creating) the reservation directory, verified like the
/// log root itself.
pub(crate) fn reserved_dir(
    root: &crate::guest_anchor::AnchoredDir,
) -> Result<crate::guest_anchor::AnchoredDir> {
    open_verified_dir(&root.path().join(RESERVED_DIR))
        .map_err(|error| CistellaError::Runtime(format!("operation log reservations: {error}")))
}

/// Reads live reservations under the held admission lock, reaping
/// entries whose owners are provably dead. Unknown liveness keeps
/// the entry (pessimistic, safe direction: it may refuse logging
/// spuriously but never over-admits on a guess).
/// Writes one reservation file: owner-attributed capacity the
/// opener holds until close or drop. Any failure refuses the open
/// (fail closed on the logging path only; the business operation
/// proceeds without logging).
pub(crate) fn write_reservation(
    reserved: &crate::guest_anchor::AnchoredDir,
    id: &str,
    share: u64,
) -> Result<()> {
    let mut file = reserved
        .create_file(&format!("{id}.json"))
        .map_err(|error| CistellaError::Runtime(format!("operation log reservation: {error}")))?;
    let owner = self_owner();
    let body = serde_json::json!({
        "op": id,
        "pid": std::process::id(),
        "process_start": owner.map(|o| o.starttime),
        "bytes": share,
    });
    let bytes = serde_json::to_vec(&body)
        .map_err(|error| CistellaError::Runtime(format!("operation log reservation: {error}")))?;
    file.write_all(&bytes)
        .map_err(|error| CistellaError::Runtime(format!("operation log reservation: {error}")))?;
    Ok(())
}

/// Releases one reservation, best-effort and silent. Runs on close
/// and on drop, so a torn stream never leaks accounted capacity
/// past owner death (stale entries are reaped on the next open
/// when the owner is provably dead).
pub(crate) fn release_reservation(root: &Path, id: &str) {
    let Ok(reserved) = open_verified_dir(&root.join(RESERVED_DIR)) else {
        return;
    };
    let _ = nix::unistd::unlinkat(
        Some(reserved.dirfd()),
        std::path::Path::new(&format!("{id}.json")),
        nix::unistd::UnlinkatFlags::NoRemoveDir,
    );
}

/// Bound on reservation entries scanned per open: concurrent
/// in-flight opens each hold one entry, so thousands is already
/// absurd. Past it the open is refused (logging skipped, business
/// unaffected) rather than scanning an unbounded planted listing
/// while holding the admission lock.
const MAX_RESERVED_ENTRIES: u32 = 4096;

/// Ceiling for one reservation payload: ours are ~100 bytes of
/// JSON. Past it the entry is not ours; it is charged
/// conservatively rather than read unbounded.
const MAX_RESERVATION_BYTES: u64 = 8192;

/// Reads live reservations under the held admission lock, reaping
/// entries whose owners are provably dead. Unknown liveness keeps
/// the entry (pessimistic, safe direction: it may refuse logging
/// spuriously but never over-admits on a guess). Every entry the
/// scan cannot account for — unreadable file, invalid JSON,
/// missing byte count — is charged one full reservation share,
/// never zero: uncertainty is capacity, and a listing failure
/// refuses the open rather than admitting blind. Non-regular
/// files (FIFO, directory, symlink) are skipped: our writer only
/// ever creates regular files, so they hold no capacity — but
/// the open is non-blocking and type-checked, so they can never
/// stall this scan either.
pub(crate) fn live_reserved_bytes(reserved: &crate::guest_anchor::AnchoredDir) -> Result<u64> {
    let mut total = 0u64;
    let mut seen = 0u32;
    let listing = std::fs::read_dir(reserved.path())
        .map_err(|error| CistellaError::Runtime(format!("operation log reservations: {error}")))?;
    for entry in listing {
        let entry = entry.map_err(|error| {
            CistellaError::Runtime(format!("operation log reservations: {error}"))
        })?;
        seen += 1;
        if seen > MAX_RESERVED_ENTRIES {
            return Err(CistellaError::Runtime(
                "operation log reservations exceed entry bound".to_string(),
            ));
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let raw = match nix::fcntl::openat(
            Some(reserved.dirfd()),
            std::path::Path::new(&name),
            nix::fcntl::OFlag::O_RDONLY
                | nix::fcntl::OFlag::O_NONBLOCK
                | nix::fcntl::OFlag::O_NOFOLLOW
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        ) {
            // An entry that cannot be opened (deleted or
            // permission-raced under us) is capacity we cannot
            // account for: charged, never zeroed.
            Err(_) => {
                total += RESERVATION_BYTES;
                continue;
            }
            Ok(raw) => raw,
        };
        // SAFETY: freshly opened above, owned here, wrapped exactly once.
        let mut file: std::fs::File = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw).into() };
        // fstat on the open description: FIFOs, directories and
        // symlinks are not our reservations and hold no capacity,
        // and O_NONBLOCK kept their open from stalling us. An
        // fstat failure is unknown, not proof of non-regular, so
        // it is charged rather than silently skipped.
        let is_regular = match file.metadata() {
            Ok(meta) => meta.file_type().is_file(),
            Err(_) => {
                total = total.saturating_add(RESERVATION_BYTES);
                continue;
            }
        };
        if !is_regular {
            continue;
        }
        let mut content = Vec::new();
        let readable = (&mut file)
            .take(MAX_RESERVATION_BYTES + 1)
            .read_to_end(&mut content)
            .is_ok()
            && content.len() as u64 <= MAX_RESERVATION_BYTES;
        let value: Option<serde_json::Value> = if readable {
            serde_json::from_slice(&content).ok()
        } else {
            None
        };
        // Unreadable, overlarge or invalid: one full share of
        // capacity we cannot disprove. Only a well-formed entry
        // with a live owner charges its stated bytes, and only a
        // well-formed entry with a provably dead owner is reaped.
        // Semantic validation: production reservations are one
        // fixed quantum with a full owner identity. The byte claim
        // is never trusted: any unknown or malformed entry — zero,
        // sub-share, missing, or overlarge count — is charged
        // exactly one share. A missing start identity is never
        // substituted with zero and compared: that fabricated
        // identity could read a live owner as provably dead. The
        // complete identity below serves only provable-death
        // reaping, never the amount.
        let (owner, bytes) = match value {
            Some(value) => {
                let pid = value
                    .get("pid")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok());
                let starttime = value
                    .get("process_start")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|starttime| *starttime != 0);
                let owner = match (pid, starttime) {
                    (Some(pid), Some(starttime)) => Some(Owner { pid, starttime }),
                    _ => None,
                };
                (owner, RESERVATION_BYTES)
            }
            None => (None, RESERVATION_BYTES),
        };
        let dead = match owner {
            Some(owner) => owner_alive(&owner) == Some(false),
            None => false,
        };
        if dead {
            let _ = nix::unistd::unlinkat(
                Some(reserved.dirfd()),
                std::path::Path::new(&name),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            );
            continue;
        }
        total = total.saturating_add(bytes);
    }
    Ok(total)
}
