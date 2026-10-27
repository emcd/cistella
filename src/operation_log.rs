//! Durable per-operation logging for lifecycle paths.
//!
//! Each mutating lifecycle operation (`gc`, install/start, `terminate`,
//! shared teardown) appends length-prefixed JSON records under
//! `$XDG_STATE_HOME/cistella/logs/<operation-id>/`. Later failures
//! (for example the surgery collector's post-mortem) correlate against
//! these logs instead of inferring actors from timing.
//!
//! The log records decisions without influencing them: every logging
//! failure is an observability loss only, and the business operation
//! proceeds unchanged. See the `operation-logging` OpenSpec capability
//! for the normative contract.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

use crate::error::{CistellaError, Result};

/// Maximum payload bytes of one frame (length prefix excluded).
pub const MAX_FRAME_PAYLOAD: usize = 256 * 1024;

/// Maximum accounted bytes of one operation directory.
pub const PER_OP_CAP: u64 = 1024 * 1024;

/// Hard logical budget for the whole log tree, in operation bytes.
pub const TOTAL_BUDGET: u64 = 3 * 1024 * 1024 * 1024;

/// Protected failure budget inside the tree, in operation bytes.
pub const FAILURE_RESERVE: u64 = 1024 * 1024 * 1024;

/// Success operations retained by count (newest first).
pub const SUCCESS_COUNT_CAP: usize = 2000;

/// Retention age in seconds (7 days).
pub const RETENTION_AGE_SECS: u64 = 7 * 24 * 3600;

/// Bound for one injected metadata string.
pub const INJECTED_BOUND: usize = 128;

/// Upper bound on directory entries visited by one scan.
pub const SCAN_ENTRY_CAP: usize = 100_000;

/// Bytes reserved per operation directory for metadata share.
const OP_METADATA_SHARE: u64 = 4096;

/// Capacity reserved per open operation: metadata share plus one
/// full per-operation cap. Tests pin admission at this exact
/// quantum.
/// Marker file inside an operation directory while terminal
/// writes or fsync are still pending. A prune must not unlink an
/// operation whose marker is present and whose owner is live.
const FINALIZING_MARKER: &str = ".finalizing";

/// Name of the marker `end_finalization` leaves behind by
/// renaming (never deleting) the in-progress marker: evidence
/// that a close ran finalization to completion. The pruner keys
/// only on the in-progress marker, so a finalized operation stays
/// policy-eligible.
const FINALIZED_MARKER: &str = ".finalized";

/// Fixed operation vocabulary for argv labels. Labels never
/// render command lines, so no untrusted path text lands in a
/// label.
pub const KNOWN_VERBS: &[&str] = &["gc", "install", "start", "teardown", "terminate"];

/// Name of the records file inside one operation directory.
const RECORDS_NAME: &str = "records.log";

/// Directory holding one marker file per evicted operation.
const EVICTED_DIR: &str = "evicted";

/// Returns the durable log root: `$XDG_STATE_HOME/cistella/logs` with
/// fallback to `$HOME/.local/state/cistella/logs`, then the process
/// temp dir. The fallback chain never fails; anchoring and ownership
/// are verified at open, not here.
#[must_use]
pub fn log_root() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("cistella/logs");
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local/state/cistella/logs");
    }
    std::env::temp_dir().join("cistella-logs")
}

/// Mints one operation id: framework-minted, path-safe `[a-z0-9]`,
/// fixed length.
#[must_use]
pub fn mint_op_id() -> String {
    crate::session::mint_session_id()
}

/// Reports whether a directory entry name is a valid operation id:
/// non-empty, bounded length, lowercase alphanumerics only. Prune
/// victim selection admits only names passing this check.
#[must_use]
pub fn op_id_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// Injected run metadata: untrusted claims from the environment,
/// never proof of origin. Every value is length-bounded.
#[derive(Debug, Clone, Default)]
pub struct Injected {
    /// Test-run id claim (`CISTELLA_TEST_RUN_ID`).
    pub run_id: Option<String>,
    /// Test name claim (`CISTELLA_TEST_NAME`).
    pub test_name: Option<String>,
    /// Parent operation id claim (`CISTELLA_PARENT_OP_ID`).
    pub parent_op_id: Option<String>,
    /// Exact source SHA claim (`CISTELLA_SHA`).
    pub sha: Option<String>,
}

/// Bounds one injected string, marking truncation.
/// Truncates to at most `cap` bytes on a UTF-8 boundary, with a
/// truncation marker. Never panics on multi-byte characters: the
/// cut point floors to the previous character boundary.
/// Truncates to at most `cap` bytes on a UTF-8 boundary (shared
/// by injected-claim bounding and backend-text capping).
#[must_use]
pub fn truncate_str(raw: &str, cap: usize) -> String {
    if raw.len() <= cap {
        return raw.to_string();
    }
    let mut end = cap.min(raw.len());
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = raw[..end].to_string();
    out.push_str("[truncated]");
    out
}

fn bound_claim(raw: &str) -> String {
    truncate_str(raw, INJECTED_BOUND)
}

/// Reads injected metadata from the environment.
#[must_use]
pub fn read_injected() -> Injected {
    read_injected_from(&|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
}

/// Injected-metadata intake over an explicit source, so the
/// bounding and validation pin without process-global env
/// mutation. Production passes the process environment.
#[must_use]
pub fn read_injected_from(get: &dyn Fn(&str) -> Option<String>) -> Injected {
    let claim = |key: &str| get(key).map(|value| bound_claim(&value));
    let parent_op_id = claim("CISTELLA_PARENT_OP_ID").filter(|id| op_id_valid(id));
    Injected {
        run_id: claim("CISTELLA_TEST_RUN_ID"),
        test_name: claim("CISTELLA_TEST_NAME"),
        parent_op_id,
        sha: claim("CISTELLA_SHA"),
    }
}

/// Owner identity of one operation: pid plus process start time.
/// Both are required because pids are reused; the pair is the
/// liveness primitive, never mtime guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    /// Process id of the owning writer.
    pub pid: u32,
    /// Process start time in clock ticks (`/proc` field 22).
    pub starttime: u64,
}

/// Reads one process start time from `/proc` (field 22 after the
/// comm field). Returns `None` when the process or field is
/// unreadable.
fn proc_starttime(pid: u32) -> Option<u64> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = content.rsplit(')').next()?;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// Returns the current process owner identity, or `None` when the
/// platform does not expose it.
#[must_use]
pub fn self_owner() -> Option<Owner> {
    proc_starttime(std::process::id()).map(|starttime| Owner {
        pid: std::process::id(),
        starttime,
    })
}

/// Reports whether the recorded owner is provably live, provably
/// dead, or unknowable. An absent `/proc` entry means certainly
/// dead (no PID-reuse ambiguity: reuse implies presence). Any
/// other observation failure means unknown, never dead, and the
/// caller treats unknown as live (skip, never reap or free on a
/// guess).
#[must_use]
pub fn owner_alive(owner: &Owner) -> Option<bool> {
    match std::fs::symlink_metadata(format!("/proc/{}", owner.pid)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(false),
        Err(_) => return None,
        Ok(_) => {}
    }
    proc_starttime(owner.pid).map(|now| now == owner.starttime)
}

/// Single owning writer of one operation file. No multi-writer
/// joins: concurrent operations use separate files.
pub struct Operation {
    /// Display path of the operation directory (never re-resolved).
    dir: PathBuf,
    /// Log root for best-effort prune on close.
    root: PathBuf,
    /// The single owning file handle.
    file: File,
    /// Next sequence number.
    seq: u64,
    /// Accounted bytes in this operation (frames plus prefixes).
    bytes: u64,
    /// Set once a write fails or the budget refuses: no further
    /// frames are appended behind the torn stream.
    broken: bool,
    /// Whether this handle still holds its capacity reservation.
    /// Released on close; Drop releases if close never ran.
    reservation_held: bool,
    /// Monotonic start for duration accounting.
    start: std::time::Instant,
}

/// Opens one verified directory: creates 0700, opens nofollow, and
/// checks seat-or-root ownership on the open FD.
fn open_verified_dir(path: &Path) -> Result<crate::guest_anchor::AnchoredDir> {
    crate::guest_anchor::open_state_dir(path)
        .map_err(|error| CistellaError::Runtime(format!("operation log dir: {error}")))
}

/// Opens the log root, creating missing ancestors with the same
/// anchored discipline. A fresh seat has no state chain yet;
/// the non-recursive [`open_verified_dir`] would refuse every
/// open there and no log would ever exist. Only the root uses
/// this: everything beneath it has a guaranteed parent from here.
fn open_verified_root(path: &Path) -> Result<crate::guest_anchor::AnchoredDir> {
    crate::guest_anchor::open_state_dir_all(path)
        .map_err(|error| CistellaError::Runtime(format!("operation log dir: {error}")))
}

/// Reports whether metadata describes a seat- or root-owned
/// non-symlink. Anything else is never trusted, counted, or
/// reaped: unowned or linked paths stay invisible to the log
/// budget and the pruner alike.
fn trusted_entry(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    if meta.file_type().is_symlink() {
        return false;
    }
    let uid = meta.uid();
    if uid == 0 {
        return true;
    }
    // SAFETY: getuid is inherently safe.
    unsafe { uid == nix::libc::getuid() }
}
/// Sums regular-file bytes one level under each operation
/// directory plus metadata shares, bounded by entry count. Only
/// seat- or root-owned non-symlinks are counted; anything else
/// stays invisible to the budget. Used for admission: failure
/// here skips logging, never the business operation.
fn tree_usage(root: &Path) -> Result<(u64, usize)> {
    let mut bytes = 0u64;
    let mut entries = 0usize;
    let listing = std::fs::read_dir(root)
        .map_err(|error| CistellaError::Runtime(format!("operation log scan: {error}")))?;
    for entry in listing {
        entries += 1;
        if entries > SCAN_ENTRY_CAP {
            return Err(CistellaError::Runtime(
                "operation log scan exceeds entry bound".to_string(),
            ));
        }
        let entry = entry
            .map_err(|error| CistellaError::Runtime(format!("operation log scan: {error}")))?;
        let entry_meta = std::fs::symlink_metadata(entry.path())
            .map_err(|error| CistellaError::Runtime(format!("operation log scan: {error}")))?;
        if !trusted_entry(&entry_meta) {
            continue;
        }
        if entry_meta.is_dir() {
            bytes += OP_METADATA_SHARE;
            let inner = std::fs::read_dir(entry.path())
                .map_err(|error| CistellaError::Runtime(format!("operation log scan: {error}")))?;
            for file in inner {
                entries += 1;
                if entries > SCAN_ENTRY_CAP {
                    return Err(CistellaError::Runtime(
                        "operation log scan exceeds entry bound".to_string(),
                    ));
                }
                let file = file.map_err(|error| {
                    CistellaError::Runtime(format!("operation log scan: {error}"))
                })?;
                let file_meta = std::fs::symlink_metadata(file.path()).map_err(|error| {
                    CistellaError::Runtime(format!("operation log scan: {error}"))
                })?;
                if !trusted_entry(&file_meta) || !file_meta.is_file() {
                    continue;
                }
                bytes += file_meta.len();
            }
        } else if entry_meta.is_file() {
            bytes += entry_meta.len();
        }
    }
    Ok((bytes, entries))
}

impl Operation {
    /// Opens one operation log under the production budget. See
    /// [`Operation::open_with`] for the admission contract.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` when the root cannot be
    /// verified, the budget cannot secure the open, or the start
    /// record cannot be written.
    pub fn open(
        root: &Path,
        verb: &str,
        parent: Option<&str>,
        injected: &Injected,
    ) -> Result<Self> {
        Self::open_with(root, verb, parent, injected, &prune::Budget::production())
    }

    /// Opens one operation log under the given budget with a
    /// minted id. See [`Operation::open_with_id`] for the
    /// admission contract.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` when the root cannot be
    /// verified, the admission lock is contended, the budget
    /// cannot secure the open, or the start record cannot be
    /// written.
    pub fn open_with(
        root: &Path,
        verb: &str,
        parent: Option<&str>,
        injected: &Injected,
        budget: &prune::Budget,
    ) -> Result<Self> {
        let id = mint_op_id();
        Self::open_with_id(root, verb, parent, injected, budget, &id)
    }

    /// Opens one operation log under an explicit id. The id must
    /// already satisfy [`op_id_valid`]; the reservation is
    /// registered under it before anything else is created, so a
    /// failure after reservation is still released instead of
    /// leaked. Production callers prefer [`Operation::open`], which
    /// mints the id.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` on an invalid id, an
    /// unsecurable budget, or any creation failure (the
    /// reservation is released on every error path).
    pub fn open_with_id(
        root: &Path,
        verb: &str,
        parent: Option<&str>,
        injected: &Injected,
        budget: &prune::Budget,
        id: &str,
    ) -> Result<Self> {
        // Fixed vocabulary in all builds, not just debug: an
        // unknown verb is recorded under the fixed `unknown`
        // label rather than arriving verbatim in a label.
        let verb = if KNOWN_VERBS.contains(&verb) {
            verb
        } else {
            "unknown"
        };
        let root_dir = open_verified_root(root)?;
        // Admission under a nonblocking lock: contention skips
        // logging rather than stalling the business operation.
        // Same-process concurrent opens serialize on the flock
        // like any other contender; losers skip logging.
        let _admission = admission::admission_lock(&root_dir)?;
        let (used, _) = tree_usage(root)?;
        // Reservation accounting is refusal, not zero, on any
        // accounting failure: an unreadable listing, an unreadable
        // entry, or invalid JSON charges conservatively or refuses
        // the open (logging skipped, business unaffected) rather
        // than admitting blind against the hard budget.
        let reserved_dir = admission::reserved_dir(&root_dir)?;
        let reserved = admission::live_reserved_bytes(&reserved_dir)?;
        let share = RESERVATION_BYTES;
        if used.saturating_add(reserved).saturating_add(share) > budget.total {
            return Err(CistellaError::Runtime(
                "operation log budget cannot secure open".to_string(),
            ));
        }
        if !op_id_valid(id) {
            return Err(CistellaError::Runtime(
                "operation log id is not path-safe".to_string(),
            ));
        }
        admission::write_reservation(&reserved_dir, id, share)?;
        // The lock releases here; the reservation file carries the
        // accounting from now on, so concurrent opens see it.
        drop(_admission);
        let built = (|| -> Result<Self> {
            let dir = root.join(id);
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&dir)
                    .map_err(|error| {
                        CistellaError::Runtime(format!("operation log dir: {error}"))
                    })?;
            }
            verify_dir(&dir)?;
            let records_path = dir.join(RECORDS_NAME);
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&records_path)
                .map_err(|error| {
                    CistellaError::Runtime(format!("operation log records: {error}"))
                })?;
            set_owner_only(&records_path)?;
            let wall = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut op = Self {
                dir,
                root: root.to_path_buf(),
                file,
                seq: 0,
                bytes: 0,
                broken: false,
                reservation_held: true,
                start: std::time::Instant::now(),
            };
            let start_payload = serde_json::to_vec(&start_record(
                id, verb, parent, injected, wall,
            )?)
            .map_err(|error| CistellaError::Runtime(format!("operation log encode: {error}")))?;
            op.append_bytes(&start_payload)?;
            Ok(op)
        })();
        // A failure after the reservation was written must free it:
        // without this the capacity leaks until the owner dies and
        // a later open reaps it.
        if built.is_err() {
            admission::release_reservation(root, id);
        }
        built
    }

    /// Display path of the operation directory (never re-resolved).
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Operation id: the directory name. Used to link nested
    /// operations to their parent.
    #[must_use]
    pub fn id(&self) -> &str {
        self.dir.file_name().and_then(|n| n.to_str()).unwrap_or("")
    }

    /// Appends one body record. On any failure the stream is marked
    /// broken and no further frames are appended; the caller keeps
    /// the business decision unchanged.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Runtime` when the stream is broken,
    /// the payload is oversize, the per-operation cap refuses, or
    /// the write fails.
    pub fn append(&mut self, record: &serde_json::Value) -> Result<()> {
        if self.broken {
            return Err(CistellaError::Runtime(
                "operation log stream is broken".to_string(),
            ));
        }
        let mut body = record.clone();
        body["seq"] = serde_json::Value::from(self.seq);
        body["kind"] = serde_json::Value::from("body");
        let payload = serde_json::to_vec(&body)
            .map_err(|error| CistellaError::Runtime(format!("operation log encode: {error}")))?;
        self.append_bytes(&payload)
    }

    /// Closes the operation with an outcome: best-effort terminal
    /// record (skipped when the stream is broken), fsync, then
    /// best-effort prune. Prune and close failures are silent.
    /// Opens the finalization window: creates the marker so no
    /// concurrent prune unlinks this path while the terminal write
    /// or fsync is still pending. Returns whether the path is
    /// protected; `close` skips the terminal write entirely when
    /// it is not, leaving an explicitly incomplete log.
    ///
    /// `close` calls this before the terminal write and
    /// [`Operation::end_finalization`] (which renames the marker to finalized evidence) after the fsync; the pair is
    /// also the seam the finalization pin drives directly.
    #[must_use]
    pub fn begin_finalization(&self) -> bool {
        let marker = self.dir.join(FINALIZING_MARKER);
        // A pre-existing link is left alone: the marker only needs
        // to exist, and creating through a link could truncate
        // elsewhere.
        if std::fs::symlink_metadata(&marker).is_ok() {
            return true;
        }
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&marker)
            .is_ok()
    }

    /// Closes the finalization window: renames the marker to
    /// finalized evidence. From here
    /// on the operation is policy-eligible, including in its own
    /// maintenance pass, so the caller must have finished every
    /// promised write (terminal record plus fsync) first.
    pub fn end_finalization(&self) {
        let from = self.dir.join(FINALIZING_MARKER);
        let to = self.dir.join(FINALIZED_MARKER);
        if std::fs::rename(&from, &to).is_err() {
            let _ = std::fs::remove_file(from);
        }
    }

    pub fn close(mut self, outcome: &str) {
        // Hold the admission lock across the whole finalization
        // window so a concurrent prune unlink cannot land between
        // the terminal write and the fsync. Bounded wait (50
        // attempts); on exhaustion the terminal write is SKIPPED
        // rather than performed unprotected: the log stays
        // explicitly incomplete (UNKNOWN on read) instead of
        // risking a torn finalization, and the business operation
        // is unaffected. The guard MUST drop before the
        // maintenance pass below: the admission lock is not
        // reentrant across open file descriptions, so holding it
        // through prune would make every normal close skip its
        // own evictions.
        let _finalization = open_verified_dir(&self.root)
            .ok()
            .and_then(|root| admission::admission_lock_bounded(&root, 50).ok());
        let protected = _finalization.is_some();
        if protected {
            let _ = self.begin_finalization();
        }
        if !self.broken && protected {
            let duration_ms = self.start.elapsed().as_millis() as u64;
            let end_wall = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let terminal = serde_json::json!({
                "seq": self.seq,
                "kind": "terminal",
                "outcome": outcome,
                "end_wall": end_wall,
                "duration_ms": duration_ms,
                "finalization": "locked",
            });
            match serde_json::to_vec(&terminal) {
                Ok(payload) => {
                    if self.append_bytes(&payload).is_err() {
                        self.broken = true;
                    }
                }
                Err(_) => self.broken = true,
            }
        }
        let _ = self.file.sync_all();
        // Finalization is complete once the terminal is synced:
        // the operation is policy-eligible from here on, including
        // in its own maintenance pass below.
        self.end_finalization();
        drop(_finalization);
        self.release();
        let root = self.root.clone();
        prune::prune_on_close(&root);
    }

    /// Releases this handle's capacity reservation, best-effort and
    /// silent. Safe to call repeatedly.
    fn release(&mut self) {
        if !self.reservation_held {
            return;
        }
        self.reservation_held = false;
        let Some(id) = self.dir.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        admission::release_reservation(&self.root, id);
    }

    /// Appends pre-encoded payload after reserving against the
    /// per-operation cap. Marks the stream broken on any failure.
    fn append_bytes(&mut self, payload: &[u8]) -> Result<()> {
        let frame = encode_frame(payload)?;
        if self.bytes + frame.len() as u64 > PER_OP_CAP {
            self.broken = true;
            return Err(CistellaError::Runtime(
                "operation log per-operation cap refuses append".to_string(),
            ));
        }
        if self.file.write_all(&frame).is_err() {
            self.broken = true;
            return Err(CistellaError::Runtime(
                "operation log write failed; stream stopped".to_string(),
            ));
        }
        self.bytes += frame.len() as u64;
        self.seq += 1;
        Ok(())
    }
}

/// Verifies one directory by FD: nofollow open, directory type,
/// seat-or-root ownership.
fn verify_dir(path: &Path) -> Result<()> {
    let raw = nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|errno| CistellaError::Runtime(format!("operation log dir: {errno}")))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let fd: std::os::fd::OwnedFd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    let stat = nix::sys::stat::fstat(std::os::fd::AsFd::as_fd(&fd).as_raw_fd())
        .map_err(|errno| CistellaError::Runtime(format!("operation log dir: {errno}")))?;
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    if stat.st_uid != seat && stat.st_uid != 0 {
        return Err(CistellaError::Runtime(
            "operation log dir must be seat- or root-owned".to_string(),
        ));
    }
    Ok(())
}

/// Hardens one file to owner-only access.
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| CistellaError::Runtime(format!("operation log records: {error}")))
}

/// Builds the start record for one operation.
/// Operational role for an envelope actor: the fixed vocabulary
/// behind the `role` field. Unknown verbs map to `unknown`, never
/// verbatim.
fn role_for(verb: &str) -> &'static str {
    match verb {
        "gc" => "orphan-reaping",
        "install" => "installing",
        "start" => "starting",
        "terminate" => "terminating",
        "teardown" => "tearing-down",
        _ => "unknown",
    }
}

fn start_record(
    id: &str,
    verb: &str,
    parent: Option<&str>,
    injected: &Injected,
    wall: u64,
) -> Result<serde_json::Value> {
    let owner = self_owner();
    Ok(serde_json::json!({
        "seq": 0u64,
        "kind": "start",
        "op": id,
        "parent": parent,
        "actor": verb,
        "role": role_for(verb),
        "pid": std::process::id(),
        "process_start": owner.map(|o| o.starttime),
        "argv_label": verb,
        "start_wall": wall,
        "run_id": injected.run_id,
        "test_name": injected.test_name,
        "parent_claim": injected.parent_op_id,
        "sha": injected.sha,
    }))
}

impl Drop for Operation {
    /// Best-effort reservation release. Never panics: every step
    /// is silent, so a torn stream never leaks accounted capacity
    /// past owner death (and any stale entry is reaped on the
    /// next open when the owner is provably dead).
    fn drop(&mut self) {
        self.release();
    }
}

/// Pruning: victim selection, safe deletion, eviction accounting.
pub mod admission;
pub mod codec;
pub mod emit;
pub mod prune;

pub use admission::RESERVATION_BYTES;
pub use codec::{Outcome, ReadResult, decode_frames, encode_frame, read_operation};
pub use emit::{
    Classify, Observed, cap_str, open_best_effort, record_gc_classify, record_gc_phase,
    record_gc_reap, record_gc_scan, record_step, scoped, scoped_at,
};
pub use prune::{Budget, PruneReport, prune, prune_on_close, prune_with};
