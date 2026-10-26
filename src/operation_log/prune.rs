//! Pruning: victim selection, safe deletion, eviction accounting.
//!
//! Victim selection runs over completed AND inactive-incomplete
//! operations; active writers are skipped; every deletion is
//! dirfd-relative and every eviction leaves an accounting marker.

use std::io::Write;
use std::os::fd::FromRawFd;
use std::path::Path;

use super::codec::read_outcome;
use super::{
    EVICTED_DIR, MAX_FRAME_PAYLOAD, Outcome, Owner, PER_OP_CAP, RECORDS_NAME, Result,
    SCAN_ENTRY_CAP, decode_frames, op_id_valid, owner_alive,
};

/// Pruning budget: production defaults mirror the normative caps;
/// tests pin the same policy at small scale through this struct.
#[derive(Debug, Clone)]
pub struct Budget {
    /// Hard logical tree budget, in operation bytes.
    pub total: u64,
    /// Protected failure budget inside the tree, in bytes.
    pub reserve: u64,
    /// Success operations retained by count (newest first).
    pub success_cap: usize,
    /// Retention age in seconds.
    pub age_secs: u64,
}

impl Budget {
    /// Production budget from the normative requirements.
    #[must_use]
    pub fn production() -> Self {
        Self {
            total: super::TOTAL_BUDGET,
            reserve: super::FAILURE_RESERVE,
            success_cap: super::SUCCESS_COUNT_CAP,
            age_secs: super::RETENTION_AGE_SECS,
        }
    }
}

/// One prune run's observable result.
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    /// Operation directories scanned.
    pub scanned: usize,
    /// Operations evicted.
    pub evicted: usize,
    /// Evictions attempted but failed (observability loss only).
    pub eviction_failures: usize,
    /// Victims skipped by the unlink-time recheck (still
    /// protected: observed after selection, before removal).
    pub skipped: usize,
    /// Bytes reclaimed by successful evictions.
    pub bytes_reclaimed: u64,
}

/// One prune victim: selected by [`select_victims`], removed by
/// [`evict`]. Splitting selection from removal is what lets a test
/// plant a finalizing marker in between and prove the unlink-time
/// recheck skips it.
#[derive(Debug, Clone)]
pub struct Victim {
    /// Operation directory name.
    pub name: String,
    /// Why it was selected (accounting and diagnostics).
    pub reason: &'static str,
}

/// One scanned operation directory as the pruner sees it.
struct OpView {
    name: String,
    bytes: u64,
    mtime_secs: u64,
    outcome: Outcome,
    owner: Option<Owner>,
    start_readable: bool,
    /// A finalizing marker was present at inspection: terminal
    /// writes or fsync may still be pending.
    has_marker: bool,
}

/// Inspects one operation directory: parses the start record for
/// the owner and reads the outcome. Unreadable directories read
/// UNKNOWN with no owner; the caller treats unknown liveness as
/// active (skip) unless the age bound has passed.
fn inspect_op(root: &Path, name: &str, now_secs: u64) -> Option<OpView> {
    let dir = root.join(name);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if !super::trusted_entry(&meta) || !meta.is_dir() {
        return None;
    }
    // A present marker means terminal writes or fsync may still be
    // pending; any symlink here only ever skips (safe direction).
    let has_marker = std::fs::symlink_metadata(dir.join(super::FINALIZING_MARKER)).is_ok();
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(now_secs);
    let records_path = dir.join(RECORDS_NAME);
    // A missing records file is not an error: the operation never
    // wrote anything, so there is no evidence to read. The
    // directory itself is still a candidate (aged empties are
    // reaped; fresh ones are skipped as possibly mid-creation).
    let file_meta = match std::fs::symlink_metadata(&records_path) {
        Ok(meta) => meta,
        Err(_) => {
            return Some(OpView {
                name: name.to_string(),
                bytes: 0,
                mtime_secs,
                outcome: Outcome::Unknown,
                owner: None,
                start_readable: false,
                has_marker,
            });
        }
    };
    if !super::trusted_entry(&file_meta) || !file_meta.is_file() {
        return Some(OpView {
            name: name.to_string(),
            bytes: 0,
            mtime_secs,
            outcome: Outcome::Unknown,
            owner: None,
            start_readable: false,
            has_marker,
        });
    }
    let bytes = file_meta.len();
    // Bounded, non-blocking, verified-regular read: a FIFO or
    // other non-regular file planted or swapped in after the stat
    // above cannot block this scan or over-allocate it; it reads
    // as an unreadable candidate (UNKNOWN, skipped or aged out by
    // the normal rules) rather than stalling the pruner.
    let content = match super::codec::read_bounded_regular(
        &records_path,
        PER_OP_CAP + MAX_FRAME_PAYLOAD as u64,
    ) {
        Ok(content) => content,
        Err(_) => {
            return Some(OpView {
                name: name.to_string(),
                bytes,
                mtime_secs,
                outcome: Outcome::Unknown,
                owner: None,
                start_readable: false,
                has_marker,
            });
        }
    };
    let (records, _) = decode_frames(&content);
    let outcome = read_outcome(&records);
    let owner = records.first().and_then(|record| {
        let pid = record.get("pid")?.as_u64()? as u32;
        let starttime = record.get("process_start")?.as_u64()?;
        Some(Owner { pid, starttime })
    });
    Some(OpView {
        name: name.to_string(),
        bytes,
        mtime_secs,
        outcome,
        owner,
        start_readable: true,
        has_marker,
    })
}

/// Reports whether one scanned operation is actively written.
/// The finalizing marker wins first: a present marker with a live
/// owner means terminal writes or fsync may still be pending, so
/// the operation is protected regardless of what the records read
/// (a completed-looking prefix with a pending fsync must not lose
/// its path). Marker plus dead owner falls through to the normal
/// rules, so stale markers never immortalize anything. Otherwise a
/// terminally closed operation is eligible regardless of owner
/// liveness, while an incomplete operation with a live owner - or
/// unknowable liveness within the age bound - is skipped. Unknown
/// liveness reads as active (skip), never as proof of death.
fn is_active(view: &OpView, now_secs: u64, age_secs: u64) -> bool {
    if view.has_marker {
        return match view.owner {
            Some(owner) => owner_alive(&owner).unwrap_or(true),
            None => now_secs.saturating_sub(view.mtime_secs) <= age_secs,
        };
    }
    if matches!(view.outcome, Outcome::Completed(_)) {
        return false;
    }
    match view.owner {
        Some(owner) => match owner_alive(&owner) {
            Some(alive) => alive,
            None => now_secs.saturating_sub(view.mtime_secs) <= age_secs,
        },
        None => {
            if !view.start_readable {
                return now_secs.saturating_sub(view.mtime_secs) <= age_secs;
            }
            false
        }
    }
}

/// Eviction outcome for one victim directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnlinkOutcome {
    /// Unlinked; carries reclaimed bytes.
    Evicted(u64),
    /// Left in place by the unlink-time recheck.
    Skipped,
    /// Removal failed (observability loss only).
    Failed,
}

/// Deletes one operation directory dirfd-relative, with an
/// unlink-time recheck under the admission lock: the victim is
/// re-inspected, and an operation that grew a finalizing marker
/// with a live owner since selection is skipped instead of
/// unlinked. The lock makes the recheck-and-unlink atomic with
/// respect to a concurrent finalization window; a contended lock
/// defers the victim instead of failing it. Returns reclaimed
/// bytes on eviction.
fn remove_op(
    root: &crate::guest_anchor::AnchoredDir,
    name: &str,
    now_secs: u64,
    age_secs: u64,
) -> UnlinkOutcome {
    use nix::unistd::{UnlinkatFlags, unlinkat};
    let _admission = match super::admission::admission_lock(root) {
        Ok(guard) => guard,
        Err(_) => return UnlinkOutcome::Skipped,
    };
    let fresh = match inspect_op(root.path(), name, now_secs) {
        Some(view) => view,
        None => return UnlinkOutcome::Skipped,
    };
    if is_active(&fresh, now_secs, age_secs) {
        return UnlinkOutcome::Skipped;
    }
    let root_raw = root.dirfd();
    let op_raw = match nix::fcntl::openat(
        Some(root_raw),
        std::path::Path::new(name),
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    ) {
        Ok(raw) => raw,
        Err(_) => return UnlinkOutcome::Failed,
    };
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let op_fd: std::os::fd::OwnedFd = unsafe { std::os::fd::OwnedFd::from_raw_fd(op_raw) };
    use std::os::fd::AsRawFd;
    let unlink = |file: &str| {
        unlinkat(
            Some(op_fd.as_raw_fd()),
            std::path::Path::new(file),
            UnlinkatFlags::NoRemoveDir,
        )
    };
    let _ = unlink(RECORDS_NAME);
    // The finalizing marker, if any, leaves with the operation:
    // deletion never strands marker files behind.
    let _ = unlink(super::FINALIZING_MARKER);
    // Finalized evidence leaves with the operation as well: the
    // marker is a per-operation file, never shared state.
    let _ = unlink(super::FINALIZED_MARKER);
    match unlinkat(
        Some(root_raw),
        std::path::Path::new(name),
        UnlinkatFlags::RemoveDir,
    ) {
        Ok(()) => UnlinkOutcome::Evicted(fresh.bytes),
        Err(_) => UnlinkOutcome::Failed,
    }
}

/// Records one eviction as a marker file under `<root>/evicted/`,
/// best-effort: the loss stays observable even when the marker
/// write fails (the prune report counts the attempt).
fn record_eviction(root: &crate::guest_anchor::AnchoredDir, name: &str, reason: &str) {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = serde_json::json!({
        "evicted": name,
        "reason": reason,
        "at_wall": now_secs,
    });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let dir_result = (|| -> std::result::Result<(), String> {
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.path().join(EVICTED_DIR))
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else {
                        Err(error.to_string())
                    }
                })?;
        }
        let raw = nix::fcntl::openat(
            Some(root.dirfd()),
            Path::new(EVICTED_DIR),
            nix::fcntl::OFlag::O_RDONLY
                | nix::fcntl::OFlag::O_NOFOLLOW
                | nix::fcntl::OFlag::O_DIRECTORY
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|errno| errno.to_string())?;
        // SAFETY: freshly opened above, owned here, wrapped exactly once.
        let fd: std::os::fd::OwnedFd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        use std::os::fd::AsRawFd;
        // No follow, no blocking open, regular file only: a planted
        // symlink truncates nothing outside the log tree, a planted
        // FIFO cannot hang the close path, and a non-regular file is
        // refused outright.
        let file_raw = nix::fcntl::openat(
            Some(fd.as_raw_fd()),
            Path::new(&format!("{name}.json")),
            nix::fcntl::OFlag::O_WRONLY
                | nix::fcntl::OFlag::O_CREAT
                | nix::fcntl::OFlag::O_TRUNC
                | nix::fcntl::OFlag::O_NOFOLLOW
                | nix::fcntl::OFlag::O_NONBLOCK
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .map_err(|errno| errno.to_string())?;
        // SAFETY: freshly opened above, owned here, wrapped exactly once.
        let file: std::fs::File = unsafe { std::os::fd::OwnedFd::from_raw_fd(file_raw).into() };
        // Refuse non-regular files outright: with O_NOFOLLOW the open
        // itself cannot traverse a symlink, and this check drops FIFOs,
        // sockets, and directories before any byte is written.
        {
            use std::os::unix::fs::FileTypeExt;
            let bad = file
                .metadata()
                .map(|meta| {
                    let file_type = meta.file_type();
                    file_type.is_symlink()
                        || file_type.is_fifo()
                        || file_type.is_socket()
                        || !file_type.is_file()
                })
                .unwrap_or(true);
            if bad {
                return Err("operation log marker is not a regular file".to_string());
            }
        }
        let mut file = file;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        Ok(())
    })();
    let _ = dir_result;
}

/// Prunes the log tree under the total budget: success operations
/// beyond the count cap or the age bound, failure and incomplete
/// operations beyond the failure reserve or the age bound, oldest
/// first, with one accounting marker per eviction. Active
/// operations are skipped; the scan is bounded; no lifecycle locks
/// are taken. Failures are silent (observability loss only).
/// Prunes the log tree under one budget (production or test scale).
#[must_use]
/// The outcome of [`select_victims`]: victims in oldest-first
/// order plus how many candidates were examined.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Eviction victims, oldest first.
    pub victims: Vec<Victim>,
    /// Candidate operations examined.
    pub scanned: usize,
}

/// Selects eviction victims without mutating anything: scans the
/// log tree, classifies every candidate, and returns the victims in
/// oldest-first order. Deterministic over the same tree state.
pub fn select_victims(root: &Path, budget: &Budget) -> Selection {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut views: Vec<OpView> = Vec::new();
    let listing = match std::fs::read_dir(root) {
        Ok(listing) => listing,
        Err(_) => return Selection::default(),
    };
    let mut scanned_entries = 0usize;
    for entry in listing.flatten() {
        scanned_entries += 1;
        if scanned_entries > SCAN_ENTRY_CAP {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == EVICTED_DIR {
            continue;
        }
        if !op_id_valid(&name) {
            continue;
        }
        if let Some(view) = inspect_op(root, &name, now_secs) {
            views.push(view);
        }
    }
    let aged = |view: &OpView| now_secs.saturating_sub(view.mtime_secs) > budget.age_secs;
    let mut success: Vec<&OpView> = Vec::new();
    let mut failure: Vec<&OpView> = Vec::new();
    for view in &views {
        // The marker rule lives inside is_active and applies
        // regardless of outcome: a completed-looking operation
        // with a live owner and a present marker is still
        // finalizing, while a terminally closed one without a
        // marker is an ordinary victim.
        if is_active(view, now_secs, budget.age_secs) {
            continue;
        }
        match &view.outcome {
            // Only literal success counts as success: failures
            // and every unknown completed outcome accrue to the
            // protected failure class rather than silently
            // passing as success.
            Outcome::Completed(outcome) if outcome == "success" => success.push(view),
            _ => failure.push(view),
        }
    }
    let mut victims: Vec<(&OpView, &'static str)> = Vec::new();
    let mut by_mtime = success.clone();
    by_mtime.sort_by_key(|view| view.mtime_secs);
    let keep = by_mtime.len().saturating_sub(budget.success_cap);
    for view in by_mtime.iter().take(keep) {
        victims.push((view, "success-count"));
    }
    for view in &success {
        if aged(view) && !victims.iter().any(|(v, _)| v.name == view.name) {
            victims.push((view, "success-age"));
        }
    }
    let reserve_used: u64 = failure.iter().map(|view| view.bytes).sum();
    if reserve_used > budget.reserve {
        let mut ordered = failure.clone();
        ordered.sort_by_key(|view| view.mtime_secs);
        let mut freed = 0u64;
        let excess = reserve_used - budget.reserve;
        for view in ordered {
            if freed >= excess {
                break;
            }
            if !victims.iter().any(|(v, _)| v.name == view.name) {
                freed += view.bytes;
                victims.push((view, "failure-reserve"));
            }
        }
    }
    for view in &failure {
        if aged(view) && !victims.iter().any(|(v, _)| v.name == view.name) {
            victims.push((view, "failure-age"));
        }
    }
    victims.sort_by_key(|(view, _)| view.mtime_secs);
    Selection {
        scanned: views.len(),
        victims: victims
            .into_iter()
            .map(|(view, reason)| Victim {
                name: view.name.clone(),
                reason,
            })
            .collect(),
    }
}

/// Evicts selected victims with an unlink-time recheck under the
/// admission lock: each victim is re-inspected, and an operation
/// that grew a finalizing marker with a live owner since
/// selection is skipped instead of unlinked. The lock makes the
/// recheck-and-unlink atomic with respect to a concurrent
/// finalization window; a contended lock defers the victim
/// (counted as skipped, never as a failure).
pub fn evict(root: &Path, victims: &[Victim], budget: &Budget) -> PruneReport {
    let mut report = PruneReport::default();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let anchored = match super::open_verified_dir(root) {
        Ok(dir) => dir,
        Err(_) => return report,
    };
    for victim in victims {
        match remove_op(&anchored, &victim.name, now_secs, budget.age_secs) {
            UnlinkOutcome::Evicted(bytes) => {
                report.evicted += 1;
                report.bytes_reclaimed += bytes;
                record_eviction(&anchored, &victim.name, victim.reason);
            }
            UnlinkOutcome::Skipped => {
                report.skipped += 1;
            }
            UnlinkOutcome::Failed => report.eviction_failures += 1,
        }
    }
    report
}

/// Prunes the log tree under the given budget: select, evict,
/// then age out accounting markers. Failures are silent
/// (observability loss only).
#[must_use]
pub fn prune_with(root: &Path, budget: &Budget) -> PruneReport {
    let mut report = PruneReport::default();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let anchored = match super::open_verified_dir(root) {
        Ok(dir) => dir,
        Err(_) => return report,
    };
    let selection = select_victims(root, budget);
    report.scanned = selection.scanned;
    let _ = now_secs;
    let evicted = evict(root, &selection.victims, budget);
    report.evicted = evicted.evicted;
    report.skipped = evicted.skipped;
    report.eviction_failures = evicted.eviction_failures;
    report.bytes_reclaimed = evicted.bytes_reclaimed;
    let _ = prune_eviction_markers(&anchored, now_secs, budget.age_secs);
    report
}

fn prune_eviction_markers(
    root: &crate::guest_anchor::AnchoredDir,
    now_secs: u64,
    age_secs: u64,
) -> Result<()> {
    use nix::unistd::{UnlinkatFlags, unlinkat};
    let raw = nix::fcntl::openat(
        Some(root.dirfd()),
        Path::new(EVICTED_DIR),
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    );
    let raw = match raw {
        Ok(raw) => raw,
        Err(_) => return Ok(()),
    };
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let fd: std::os::fd::OwnedFd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    use std::os::fd::AsRawFd;
    let mut scanned = 0usize;
    let mut entries: Vec<(String, u64, bool)> = Vec::new();
    if let Ok(listing) = std::fs::read_dir(root.path().join(EVICTED_DIR)) {
        for entry in listing.flatten() {
            scanned += 1;
            if scanned > SCAN_ENTRY_CAP {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let is_link = entry.file_type().map(|t| t.is_symlink()).unwrap_or(true);
            // lstat, never follow: a planted link is never read.
            let mtime = std::fs::symlink_metadata(root.path().join(EVICTED_DIR).join(&name))
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(now_secs);
            entries.push((name, mtime, is_link));
        }
    }
    for (name, mtime, is_link) in entries {
        if now_secs.saturating_sub(mtime) <= age_secs {
            continue;
        }
        if !name.ends_with(".json") || name.contains('/') {
            continue;
        }
        // Never follow a planted link, even for a marker read.
        if is_link {
            continue;
        }
        let _ = unlinkat(
            Some(fd.as_raw_fd()),
            Path::new(&name),
            UnlinkatFlags::NoRemoveDir,
        );
    }
    Ok(())
}

/// Prunes the log tree under the production budget.
///
/// See [`prune_with`] for the policy; tests pin the same policy at
/// small scale through [`Budget`].
#[must_use]
pub fn prune(root: &Path) -> PruneReport {
    prune_with(root, &Budget::production())
}

/// Best-effort prune on operation close: every failure is silent.
pub fn prune_on_close(root: &Path) {
    let _ = prune(root);
}
