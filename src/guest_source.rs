//! Registered-guest source resolution: absolute paths and bounded
//! fetch into content-addressed hash slots.
//!
//! Both sources share the same-FD anchor rule from
//! [`crate::guest_anchor`]: the digest is computed over the
//! nofollow-opened bounded FD whose object is exec'd,
//! ownership/mode enforced on the open FD, cache hits re-verified
//! on every use. Fetch transport is untrusted by design (the
//! registered hash anchors trust); rotation names a new slot, so
//! upstream changes require a registry edit.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::fcntl::OFlag;

use crate::error::{CistellaError, Result};
use crate::framework::protocol::PROTOCOL_MAJOR;
use crate::guest_anchor::{PinnedBinary, begin_anchored, openat_anchored, sha256_bytes};
use crate::guest_registry::{GuestSource, HostEnv, RegisteredGuest};

/// Read ceiling for guest binaries at verify time: sized for
/// debug binaries with symbols (tens of MB; release guests are
/// ~635KB) — mirrors the hook-artifact registry ceiling. The
/// security properties (nofollow, regular-file, fstat bind, digest
/// compare) do not depend on it.
pub const MAX_GUEST_BYTES: u64 = 64 * 1024 * 1024;

/// Resolves a registered absolute path source: refuses relative
/// paths (a relative guest would resolve against the conduct
/// worktree), canonicalizes the parent chain fully with
/// symlink-final refusal (the final component opens nofollow —
/// intermediate directory symlinks resolve identically at exec
/// because spawn binds the open FD, never the path), refuses
/// world-writable ancestor directories, then verifies the opened
/// FD's digest against `expected`.
///
/// Missing or non-executable paths refuse typed, mirroring
/// [`crate::framework::discovery::discover_in`] errors.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on relative, ancestor,
/// symlink, ownership, mode, or digest refusal, and
/// `CistellaError::Io` on open/read/stat failure.
pub fn resolve_path_source(path: &Path, expected: &[u8; 32]) -> Result<PinnedBinary> {
    if !path.is_absolute() {
        return Err(CistellaError::Profile(format!(
            "registered guest path must be absolute: {}",
            path.display()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        CistellaError::Profile(format!(
            "registered guest path has no parent: {}",
            path.display()
        ))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        CistellaError::Profile(format!(
            "registered guest path names no file: {}",
            path.display()
        ))
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|_| {
        CistellaError::Profile(format!(
            "registered guest binary absent: {}",
            path.display()
        ))
    })?;
    check_ancestors(&canonical_parent, path)?;
    let canonical = canonical_parent.join(file_name);
    let Some(open) = begin_anchored(&canonical, MAX_GUEST_BYTES)? else {
        return Err(CistellaError::Profile(format!(
            "registered guest binary absent: {}",
            path.display()
        )));
    };
    open.finish_binary(expected)
}

/// Refuses unsafe ancestor directories under the canonicalized
/// parent: a writable ancestor lets an untrusted actor swap the
/// final component's directory entry (the FD pins the object
/// post-open, but the pre-open lookup must not cross
/// untrusted-writable ground — same rationale as the ownership
/// gate: everyone else is refused by bits).
///
/// Sticky exception with directory-owner reasoning: a
/// world-writable directory WITH the sticky bit still lets its
/// DIRECTORY OWNER remove or rename anyone's entries — sticky
/// restrains non-owners only. So a sticky ancestor passes only
/// when its owner is trusted (seat uid or root, the same actors
/// the file-ownership gate trusts): `/tmp` (root-owned, mode
/// 1777) passes, a sticky dir owned by another uid refuses. A
/// non-sticky world-writable ancestor refuses regardless of
/// owner. The exception is load-bearing (seat scratch roots are
/// sticky shared dirs) and admits exactly the configurations
/// where no untrusted actor can swap the entry.
fn check_ancestors(canonical_parent: &Path, original: &Path) -> Result<()> {
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let mut current = canonical_parent;
    loop {
        let stat = std::fs::symlink_metadata(current).map_err(|_| {
            CistellaError::Profile(format!(
                "registered guest binary absent: {}",
                original.display()
            ))
        })?;
        use std::os::unix::fs::MetadataExt;
        match ancestor_verdict(stat.mode(), stat.uid(), seat) {
            AncestorVerdict::Pass => {}
            AncestorVerdict::WorldWritable => {
                return Err(CistellaError::Profile(format!(
                    "registered guest ancestor is world-writable: {}",
                    current.display()
                )));
            }
            AncestorVerdict::StickyUntrusted => {
                return Err(CistellaError::Profile(format!(
                    "registered guest ancestor is sticky but not seat-owned: {}",
                    current.display()
                )));
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    Ok(())
}

/// Traversal-safety verdict for one ancestor directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AncestorVerdict {
    /// Safe to traverse (not world-writable, or sticky with a
    /// trusted owner).
    Pass,
    /// Non-sticky world-writable: any uid swaps entries.
    WorldWritable,
    /// Sticky world-writable with an untrusted owner: the
    /// directory owner swaps entries.
    StickyUntrusted,
}

/// Pure traversal-safety predicate for one ancestor directory
/// (unit-pinned directly): non-world-writable passes;
/// non-sticky world-writable refuses (any uid swaps entries);
/// sticky world-writable passes only for trusted owners (seat or
/// root — only the directory owner can still swap entries, so
/// the owner must be trusted). Public so the truth table pins
/// directly (see `ancestor_verdict_truth_table`).
pub fn ancestor_verdict(mode: u32, uid: u32, seat: u32) -> AncestorVerdict {
    if mode & 0o002 == 0 {
        return AncestorVerdict::Pass;
    }
    if mode & 0o1000 == 0 {
        return AncestorVerdict::WorldWritable;
    }
    if uid == seat || uid == 0 {
        AncestorVerdict::Pass
    } else {
        AncestorVerdict::StickyUntrusted
    }
}

/// Resolves the fetch-slot cache directory:
/// `${XDG_CACHE_HOME}/cistella/guest-cache`, falling back to
/// `$HOME/.cache/cistella/guest-cache`. Same strictness as the
/// registry path: a nonempty `XDG_CACHE_HOME` must be absolute; a
/// missing `HOME` refuses.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on relative XDG or missing HOME.
pub fn cache_dir(env: &HostEnv) -> Result<PathBuf> {
    let base = match env.xdg_cache_home.as_deref().filter(|s| !s.is_empty()) {
        Some(dir) => {
            let path = PathBuf::from(dir);
            if !path.is_absolute() {
                return Err(CistellaError::Profile(
                    "XDG_CACHE_HOME must be absolute for registry resolution".to_string(),
                ));
            }
            path
        }
        None => {
            let home = env
                .home
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    CistellaError::Profile("HOME not set for registry resolution".to_string())
                })?;
            PathBuf::from(home).join(".cache")
        }
    };
    Ok(base.join("cistella/guest-cache"))
}

/// Resolution inputs for one registered guest: the framework-owned
/// fetch cache plus fetch bounds. Explicit (no env, no globals) so
/// tests pin bounds without process state.
#[derive(Debug, Clone)]
pub struct ResolveCtx {
    /// Framework-owned hash-slot cache directory.
    pub cache_dir: PathBuf,
    /// Max fetched bytes per fetch.
    pub fetch_max_bytes: u64,
    /// Whole-request deadline per fetch.
    pub fetch_timeout: Duration,
}

impl ResolveCtx {
    /// Builds resolution inputs with the default fetch bounds (the
    /// guest binary ceiling, one-minute deadline).
    #[must_use]
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            fetch_max_bytes: MAX_GUEST_BYTES,
            fetch_timeout: Duration::from_secs(60),
        }
    }
}

/// Fully resolved registered guest: verified bytes plus the exact
/// role string and static argv the hosting path consumes.
#[derive(Debug)]
pub struct ResolvedGuest {
    /// Exact registry name (diagnostics).
    pub name: String,
    /// Exact hello role string the guest must advertise.
    pub role: String,
    /// Wire protocol major (already checked against the host).
    pub protocol: u32,
    /// Static guest arguments, passed verbatim to the spawn.
    pub arguments: Vec<String>,
    /// Display path for argv[0] plus diagnostics (registered path,
    /// or the hash slot for fetch sources).
    pub display: PathBuf,
    /// Verified open executable object (spawn execs its FD).
    pub pinned: PinnedBinary,
}

/// Resolves one validated entry to verified bytes: checks the
/// recorded protocol major against the host wire, then resolves by
/// source (path opens and verifies directly; fetch consults the
/// hash-slot cache, downloading on miss).
///
/// Fetch and path sources share the same-FD anchor rule: the digest
/// is computed over the nofollow-opened bounded FD whose object is
/// exec'd, ownership/mode enforced on the open FD, cache hits
/// re-verified on every use.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on protocol, path, fetch, or
/// verification refusal.
pub fn resolve(entry: &RegisteredGuest, ctx: &ResolveCtx) -> Result<ResolvedGuest> {
    if entry.protocol != PROTOCOL_MAJOR {
        return Err(CistellaError::Profile(format!(
            "registered guest {} wants protocol {}, host speaks {}",
            entry.name, entry.protocol, PROTOCOL_MAJOR
        )));
    }
    let (display, pinned) = match &entry.source {
        GuestSource::Path {
            source_path,
            digest,
        } => (
            source_path.clone(),
            resolve_path_source(source_path, digest)?,
        ),
        GuestSource::Fetch { source_url, digest } => {
            let outcome = fetch_source(source_url, digest, ctx)?;
            (outcome.slot, outcome.pinned)
        }
    };
    Ok(ResolvedGuest {
        name: entry.name.clone(),
        role: entry.role.clone(),
        protocol: entry.protocol,
        arguments: entry.arguments.clone(),
        display,
        pinned,
    })
}

/// Fetch outcome: the hash slot plus the verified open description
/// of its bytes (spawn execs the FD, never the path).
#[derive(Debug)]
pub struct FetchOutcome {
    /// Hash-slot path (diagnostics and argv[0] display only).
    pub slot: PathBuf,
    /// Verified open slot bytes.
    pub pinned: PinnedBinary,
}

/// Resolves a fetch source: the slot is content-addressed by the
/// registered hash, so rotation requires a registry edit (new hash
/// names a new slot — never silent pickup).
///
/// A present slot is re-verified on every use (a hash-named mutable
/// file is not authentication): a poisoned slot refuses with a
/// typed verification error instead of refetching — local
/// tampering is a fail-closed signal, not a cache miss. On miss the
/// URL is fetched bounded (max bytes plus whole-request deadline),
/// staged to a private temp file inside the cache directory, and
/// renamed into the slot atomically (no partial entries); the slot
/// is re-opened and re-verified after the rename (closing the
/// stage-then-read window) and its FD is what spawns. A digest
/// mismatch refuses pre-create and executes nothing.
///
/// Transport is untrusted by design (the hash anchors trust), so
/// both http and https URLs fetch; integrity never depends on TLS.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on cache, fetch, bound,
/// status, or verification refusal.
pub fn fetch_source(url: &str, expected: &[u8; 32], ctx: &ResolveCtx) -> Result<FetchOutcome> {
    prepare_cache_dir(&ctx.cache_dir)?.fetch(url, expected, ctx.fetch_max_bytes, ctx.fetch_timeout)
}

/// Creates the framework-owned cache directory (0700, unconditionally
/// — a pre-existing dir keeps no looser mode) and returns the
/// open-directory guard all cache operations run relative to.
///
/// The leaf opens nofollow: a symlink at the cache path refuses
/// (ELOOP) instead of having its target chmodded, and a
/// non-directory refuses (ENOTDIR) — `create_dir_all` follows
/// symlinks, so the open is the enforcement point. Public so
/// tests can hold the guard across a path swap (the substitution
/// pin would be untestable if guard creation and use were one
/// inseparable call).
///
/// # Errors
///
/// Returns `CistellaError::Profile` on symlink, non-directory, or
/// ownership refusal, and `CistellaError::Io` on create/open/stat
/// failure.
pub fn prepare_cache_dir(dir: &Path) -> Result<CacheDirGuard> {
    std::fs::create_dir_all(dir)
        .map_err(|e| CistellaError::Profile(format!("registry cache {}: {e}", dir.display())))?;
    // Open the leaf itself nofollow: a symlink at the cache path
    // refuses here (ELOOP) instead of having its target chmodded,
    // and a non-directory refuses (ENOTDIR) — `create_dir_all`
    // follows symlinks, so the open is the enforcement point.
    let raw = nix::fcntl::open(
        dir,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|errno| {
        if errno == nix::errno::Errno::ELOOP {
            CistellaError::Profile(format!(
                "registry cache {} must not be a symlink",
                dir.display()
            ))
        } else if errno == nix::errno::Errno::ENOTDIR {
            // O_NOFOLLOW|O_DIRECTORY reports a symlink-to-dir as
            // ENOTDIR rather than ELOOP: disambiguate for the
            // diagnostic only (the nofollow open already refused —
            // no trust decision rides this second look).
            let symlink = std::fs::symlink_metadata(dir)
                .map(|stat| stat.file_type().is_symlink())
                .unwrap_or(false);
            if symlink {
                CistellaError::Profile(format!(
                    "registry cache {} must not be a symlink",
                    dir.display()
                ))
            } else {
                CistellaError::Profile(format!(
                    "registry cache {} must be a directory",
                    dir.display()
                ))
            }
        } else {
            CistellaError::Io(std::io::Error::from(errno))
        }
    })?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
    let stat = nix::sys::stat::fstat(fd.as_raw_fd())
        .map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    if stat.st_uid != seat && stat.st_uid != 0 {
        return Err(CistellaError::Profile(format!(
            "registry cache {} must be seat- or root-owned",
            dir.display()
        )));
    }
    // fchmod on the open FD, never path chmod: the mode lands on
    // the verified directory even if the path is substituted
    // concurrently.
    nix::sys::stat::fchmod(
        fd.as_raw_fd(),
        nix::sys::stat::Mode::from_bits_truncate(0o700),
    )
    .map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
    Ok(CacheDirGuard {
        fd,
        path: dir.to_path_buf(),
    })
}

/// Open-directory guard pinning the verified cache directory:
/// every stage and slot operation runs directory-FD-relative to
/// the held description (openat/renameat/unlinkat), so no path
/// operation can be substituted between check and use — there is
/// no check-then-use gap because there is no path re-resolution.
/// The FD names the ownership-checked, mode-hardened directory
/// even if the path is renamed away or symlinked elsewhere
/// mid-fetch.
#[derive(Debug)]
pub struct CacheDirGuard {
    /// Open verified cache directory (all operations relative).
    fd: OwnedFd,
    /// Path (diagnostics and display paths only — never
    /// re-resolved for trust).
    path: PathBuf,
}

impl CacheDirGuard {
    /// Raw directory FD for dirfd-relative operations.
    fn dirfd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Fetches one URL into its content-addressed slot, entirely
    /// relative to the held directory FD. A present slot
    /// re-verifies on every use (a hash-named mutable file is not
    /// authentication); a poisoned or symlinked slot refuses with
    /// a typed verification error instead of refetching. On miss
    /// the URL downloads bounded, stages to an O_EXCL temp inside
    /// the guarded directory, renames into the slot atomically,
    /// and re-opens plus re-verifies through the same FD (the
    /// spawn FD comes from the re-open).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on cache, fetch, bound,
    /// status, or verification refusal.
    pub fn fetch(
        &self,
        url: &str,
        expected: &[u8; 32],
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<FetchOutcome> {
        let slot = hex::encode(expected);
        // Direct open, never an `exists()` probe: `exists()`
        // follows symlinks and suppresses errors, so a dangling
        // symlink would read as a miss.
        match openat_anchored(self.dirfd(), &slot, MAX_GUEST_BYTES, &self.path)? {
            Some(open) => Ok(FetchOutcome {
                slot: self.path.join(&slot),
                pinned: open.finish_binary(expected)?,
            }),
            None => self.download(url, expected, &slot, max_bytes, timeout),
        }
    }

    /// Downloads one URL into its hash slot: bounded read, digest
    /// bind before first use, FD-relative temp staging, atomic
    /// FD-relative rename, then re-open plus re-verify (the spawn
    /// FD comes from the re-open, so the staged bytes and the
    /// executed bytes are one open description).
    fn download(
        &self,
        url: &str,
        expected: &[u8; 32],
        slot: &str,
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<FetchOutcome> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(5)
            .build()
            .into();
        let response = agent
            .get(url)
            .call()
            .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
        if response.status() != 200 {
            return Err(CistellaError::Profile(format!(
                "registry fetch {url}: status {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
        if bytes.len() as u64 > max_bytes {
            return Err(CistellaError::Profile(format!(
                "registry fetch {url}: exceeds size bound"
            )));
        }
        if sha256_bytes(&bytes) != *expected {
            return Err(CistellaError::Profile(format!(
                "registry fetch {url}: digest mismatch"
            )));
        }
        let mut staged = StagedTemp::create(self.dirfd(), url)?;
        staged
            .write_all(&bytes)
            .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
        staged.persist(slot, url)?;
        let Some(open) = openat_anchored(self.dirfd(), slot, MAX_GUEST_BYTES, &self.path)? else {
            return Err(CistellaError::Profile(format!(
                "registry fetch {url}: slot vanished after stage"
            )));
        };
        Ok(FetchOutcome {
            slot: self.path.join(slot),
            pinned: open.finish_binary(expected)?,
        })
    }
}

/// Counter for staging-temp name uniqueness (pid + nanos +
/// sequence; O_EXCL makes residual collisions fail-closed into a
/// retry).
static STAGE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// FD-relative staging temp: created O_EXCL inside the guarded
/// directory, renamed into the slot by renameat, unlinked on
/// every non-persisted exit (best-effort) — no stage leakage, no
/// stranger-file chmod (fchmod lands on the open temp FD).
struct StagedTemp {
    /// Guarded directory FD (rename/unlink relative).
    dirfd: RawFd,
    /// Single-component temp name.
    name: String,
    /// Open write description (closed before rename).
    file: Option<std::fs::File>,
}

impl StagedTemp {
    /// Creates an owner-only temp file inside the guarded
    /// directory: O_EXCL single-component name, fchmod 0700 on
    /// the open FD (deterministic under any umask).
    fn create(dirfd: RawFd, url: &str) -> Result<Self> {
        for _ in 0..10 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let sequence = STAGE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name = format!("stage-{}-{nanos}-{sequence}", std::process::id());
            match nix::fcntl::openat(
                Some(dirfd),
                Path::new(&name),
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC,
                nix::sys::stat::Mode::from_bits_truncate(0o600),
            ) {
                Ok(raw) => {
                    // SAFETY: freshly opened above, owned here,
                    // wrapped exactly once.
                    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
                    // The cleanup guard exists before the first
                    // fallible step: an fchmod failure unlinks the
                    // empty stage file instead of littering it.
                    let staged = Self {
                        dirfd,
                        name,
                        file: Some(std::fs::File::from(fd)),
                    };
                    nix::sys::stat::fchmod(
                        staged
                            .file
                            .as_ref()
                            .expect("staged temp holds its file until persist")
                            .as_raw_fd(),
                        nix::sys::stat::Mode::from_bits_truncate(0o700),
                    )
                    .map_err(|errno| {
                        CistellaError::Profile(format!("registry fetch {url}: {errno}"))
                    })?;
                    return Ok(staged);
                }
                Err(nix::errno::Errno::EEXIST) => continue,
                Err(errno) => {
                    return Err(CistellaError::Profile(format!(
                        "registry fetch {url}: {errno}"
                    )));
                }
            }
        }
        Err(CistellaError::Profile(format!(
            "registry fetch {url}: temp name contention"
        )))
    }

    /// Writes the verified bytes to the open temp description.
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.file
            .as_mut()
            .expect("staged temp holds its file until persist")
            .write_all(bytes)
    }

    /// Renames the temp into the slot, both relative to the
    /// guarded directory FD: renameat never follows a final
    /// symlink component, so a planted symlink at the slot is
    /// replaced (not traversed). The write description closes;
    /// the drop removes nothing (the name is gone — unlink
    /// fails ignored).
    fn persist(&mut self, slot: &str, url: &str) -> Result<()> {
        nix::fcntl::renameat(
            Some(self.dirfd),
            Path::new(&self.name),
            Some(self.dirfd),
            Path::new(slot),
        )
        .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
        self.file.take();
        Ok(())
    }
}

impl Drop for StagedTemp {
    /// Best-effort unlink of a non-persisted temp (persisted
    /// temps unlink a missing name — ignored). No stage leakage
    /// on any error path.
    fn drop(&mut self) {
        self.file.take();
        let _ = nix::unistd::unlinkat(
            Some(self.dirfd),
            Path::new(&self.name),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        );
    }
}
