//! Tmpdir orphan sweep: session-attributable enumeration of
//! `cistella-tmp-*` slices, lease/eligibility gating, and removal.
//!
//! Split from `cleanup.rs` at the file-size limit.

use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::{
    CleanupRecord, cleanup_state_name, group_dead, open_session_state,
    open_trusted_parent, pid_alive, read_lease, seat_temp_root,
    unlink_verified_tree,
};

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
        || !session.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
        })
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
pub fn reap_tmpdir_orphans(
    live_sessions: &std::collections::HashSet<String>,
) -> Vec<String> {
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return removed;
        },
        Err(error) => {
            eprintln!(
                "warning: cleanup enumerate {}: {error}",
                root.display()
            );
            return removed;
        },
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
            Ok(true) => {},
            Ok(false) => continue,
            Err(litter) => {
                eprintln!("warning: cleanup litter: {litter}");
                continue;
            },
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
                    Err(litter) => {
                        eprintln!("warning: cleanup litter: {litter}")
                    },
                }
            },
            Ok(RecordMatch::Refused) => {
                eprintln!(
                    "warning: cleanup litter: recorded refusal stands for {}",
                    candidate.display()
                );
                continue;
            },
            Ok(RecordMatch::Unrecorded) => {
                remove_lease_file(base, &session);
                match remove_orphan(root, &name) {
                    Ok(()) => removed.push(name),
                    Err(litter) => {
                        eprintln!("warning: cleanup litter: {litter}")
                    },
                }
            },
            Ok(RecordMatch::Gone) => continue,
            Err(litter) => {
                eprintln!("warning: cleanup litter: {litter}");
                continue;
            },
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
fn orphan_eligible(
    base: &Path,
    session: &str,
) -> std::result::Result<bool, String> {
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
    let bytes = match dir
        .read_file(&name, crate::guest_anchor::MAX_STATE_BYTES)
    {
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecordMatch::Gone);
        },
        Err(error) => {
            return Err(format!("cleanup {error} {}", candidate.display()));
        },
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
                    parent_stat.dev() == record.parent_dev
                        && parent_stat.ino() == record.parent_ino
                });
            if associated {
                return Ok(RecordMatch::Admitted(record.clone()));
            }
        }
    }
    if identity_seen || records.iter().any(|record| record.path == *candidate)
    {
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
fn reap_recorded(
    candidate: &Path,
    record: &CleanupRecord,
) -> std::result::Result<(), String> {
    let parent = candidate.parent().ok_or_else(|| {
        format!(
            "cleanup admitted root has no parent: {}",
            candidate.display()
        )
    })?;
    let parent_fd =
        open_trusted_parent(parent, &candidate.display().to_string())?;
    // Association on the ACTUAL opened parent FD (not just the
    // path-metadata check at match time): the parent could have
    // been swapped between the match lookup and this open, so
    // the full record stays effective through deletion.
    let parent_open =
        nix::sys::stat::fstat(parent_fd.as_raw_fd()).map_err(|e| {
            format!("cleanup fstat parent {}: {e}", parent.display())
        })?;
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
