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
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::fcntl::OFlag;

use crate::error::{CistellaError, Result};
use crate::framework::protocol::PROTOCOL_MAJOR;
use crate::guest_anchor::{PinnedBinary, begin_anchored, sha256_bytes};
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
    /// Static guest argv, passed verbatim to the spawn.
    pub args: Vec<String>,
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
        GuestSource::Path { path, sha256 } => (path.clone(), resolve_path_source(path, sha256)?),
        GuestSource::Fetch { url, sha256 } => {
            let outcome = fetch_source(url, sha256, ctx)?;
            (outcome.slot, outcome.pinned)
        }
    };
    Ok(ResolvedGuest {
        name: entry.name.clone(),
        role: entry.role.clone(),
        protocol: entry.protocol,
        args: entry.args.clone(),
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
    // The guard holds the verified directory FD across the whole
    // fetch: stage and slot operations pin against substitution
    // through it (see `check_pinned` at both boundaries).
    let guard = prepare_cache_dir(&ctx.cache_dir)?;
    let slot = ctx.cache_dir.join(hex::encode(expected));
    // Open the slot directly — never probe with `exists()` first:
    // `exists()` follows symlinks and suppresses errors, so a
    // dangling symlink at the slot would read as absent and the
    // fetch would overwrite it instead of emitting the promised
    // poisoned-slot refusal. `begin_anchored` distinguishes ENOENT
    // (miss → download) from ELOOP/permission/non-regular
    // (typed refusal, zero HTTP requests).
    match begin_anchored(&slot, MAX_GUEST_BYTES)? {
        Some(open) => Ok(FetchOutcome {
            slot,
            pinned: open.finish_binary(expected)?,
        }),
        None => download(url, expected, ctx, &guard, &slot),
    }
}

/// Creates the framework-owned cache directory (0700, unconditionally
/// — a pre-existing dir keeps no looser mode) so staged and slotted
/// bytes stay seat-private.
fn prepare_cache_dir(dir: &Path) -> Result<CacheDirGuard> {
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
        dev: stat.st_dev,
        ino: stat.st_ino,
        path: dir.to_path_buf(),
    })
}

/// Open-file guard pinning the verified cache directory across
/// stage and slot operations: the FD names the
/// ownership-checked, mode-hardened directory even if paths move.
#[derive(Debug)]
pub struct CacheDirGuard {
    /// Open verified cache directory (held through the fetch).
    fd: OwnedFd,
    /// Device at open (substitution check).
    dev: u64,
    /// Inode at open (substitution check).
    ino: u64,
    /// Path (diagnostics only — never re-resolved for trust).
    path: PathBuf,
}

impl CacheDirGuard {
    /// Refuses when `dir` no longer names the guarded directory
    /// (same dev+ino): a substitution between guard and use
    /// refuses rather than staging into, or slotting from, a
    /// stranger. Call before staging (temp parent) and before the
    /// post-rename re-open (slot parent).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on substitution, and
    /// `CistellaError::Io` on stat failure.
    pub fn check_pinned(&self, dir: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        // The held description still names the verified directory
        // (its own dev+ino are immutable for the open description —
        // this asserts the guard wasn't constructed over a swapped
        // object and the FD stayed valid across the fetch).
        let own = nix::sys::stat::fstat(self.fd.as_raw_fd())
            .map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
        if own.st_dev as u64 != self.dev || own.st_ino as u64 != self.ino {
            return Err(CistellaError::Profile(format!(
                "registry cache {} changed under its guard",
                self.path.display()
            )));
        }
        let stat = std::fs::metadata(dir).map_err(|errno| {
            CistellaError::Profile(format!("registry cache {}: {errno}", dir.display()))
        })?;
        if stat.dev() != self.dev || stat.ino() != self.ino {
            return Err(CistellaError::Profile(format!(
                "registry cache {} was substituted during fetch",
                self.path.display()
            )));
        }
        Ok(())
    }
}

/// Downloads one URL into its hash slot: bounded read, digest bind
/// before first use, private temp staging, atomic rename, then
/// re-open plus re-verify (the spawn FD comes from the re-open, so
/// the staged bytes and the executed bytes are one open
/// description).
fn download(
    url: &str,
    expected: &[u8; 32],
    ctx: &ResolveCtx,
    guard: &CacheDirGuard,
    slot: &Path,
) -> Result<FetchOutcome> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(ctx.fetch_timeout))
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
        .take(ctx.fetch_max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
    if bytes.len() as u64 > ctx.fetch_max_bytes {
        return Err(CistellaError::Profile(format!(
            "registry fetch {url}: exceeds size bound"
        )));
    }
    if sha256_bytes(&bytes) != *expected {
        return Err(CistellaError::Profile(format!(
            "registry fetch {url}: digest mismatch"
        )));
    }
    let mut staged = tempfile::NamedTempFile::new_in(&ctx.cache_dir).map_err(|e| {
        CistellaError::Profile(format!("registry cache {}: {e}", ctx.cache_dir.display()))
    })?;
    // The staging directory is the guarded one (not a substituted
    // stranger): the temp file's parent must still name the pinned
    // directory before its bytes matter.
    let staged_parent = staged.path().parent().ok_or_else(|| {
        CistellaError::Profile(format!("registry fetch {url}: staged file has no parent"))
    })?;
    guard.check_pinned(staged_parent)?;
    staged
        .write_all(&bytes)
        .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
    // Owner-only executable before the rename: the mode travels
    // into the slot, satisfying the exec-bit gate plus the
    // seat-private posture (same 0700 as staged hook artifacts).
    std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o700))
        .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
    staged
        .persist(slot)
        .map_err(|e| CistellaError::Profile(format!("registry fetch {url}: {e}")))?;
    // The slot's parent is still the guarded directory (not a
    // substituted stranger) before the re-open that hands spawn
    // its FD.
    let slot_parent = slot.parent().ok_or_else(|| {
        CistellaError::Profile(format!("registry fetch {url}: slot has no parent"))
    })?;
    guard.check_pinned(slot_parent)?;
    let Some(open) = begin_anchored(slot, MAX_GUEST_BYTES)? else {
        return Err(CistellaError::Profile(format!(
            "registry fetch {url}: slot vanished after stage"
        )));
    };
    Ok(FetchOutcome {
        slot: slot.to_path_buf(),
        pinned: open.finish_binary(expected)?,
    })
}
