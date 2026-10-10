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
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Refuses world-writable ancestor directories under the
/// canonicalized parent: a writable ancestor lets an untrusted
/// actor swap the final component's directory entry (the FD pins
/// the object post-open, but the pre-open lookup must not cross
/// untrusted-writable ground — same rationale as the ownership
/// gate: everyone else is refused by bits).
///
/// Sticky exception: a world-writable directory WITH the sticky bit
/// set (e.g. `/tmp`, mode 1777) still refuses entry
/// removal/renaming by non-owners, so untrusted actors cannot swap
/// the entry — only the owner or root may. The exception is
/// load-bearing (seat scratch roots are sticky shared dirs) and
/// defeats exactly the entry-swap threat this check exists for.
fn check_ancestors(canonical_parent: &Path, original: &Path) -> Result<()> {
    let mut current = canonical_parent;
    loop {
        let stat = std::fs::symlink_metadata(current).map_err(|_| {
            CistellaError::Profile(format!(
                "registered guest binary absent: {}",
                original.display()
            ))
        })?;
        let mode = stat.permissions().mode();
        if mode & 0o002 != 0 && mode & 0o1000 == 0 {
            return Err(CistellaError::Profile(format!(
                "registered guest ancestor is world-writable: {}",
                current.display()
            )));
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    Ok(())
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
    prepare_cache_dir(&ctx.cache_dir)?;
    let slot = ctx.cache_dir.join(hex::encode(expected));
    if slot.exists()
        && let Some(open) = begin_anchored(&slot, MAX_GUEST_BYTES)?
    {
        return Ok(FetchOutcome {
            slot,
            pinned: open.finish_binary(expected)?,
        });
    }
    download(url, expected, ctx, &slot)
}

/// Creates the framework-owned cache directory (0700, unconditionally
/// — a pre-existing dir keeps no looser mode) so staged and slotted
/// bytes stay seat-private.
fn prepare_cache_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| CistellaError::Profile(format!("registry cache {}: {e}", dir.display())))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| CistellaError::Profile(format!("registry cache {}: {e}", dir.display())))?;
    Ok(())
}

/// Downloads one URL into its hash slot: bounded read, digest bind
/// before first use, private temp staging, atomic rename, then
/// re-open plus re-verify (the spawn FD comes from the re-open, so
/// the staged bytes and the executed bytes are one open
/// description).
fn download(url: &str, expected: &[u8; 32], ctx: &ResolveCtx, slot: &Path) -> Result<FetchOutcome> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(ctx.fetch_timeout))
        .http_status_as_error(false)
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
