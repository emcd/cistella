//! Framework-owned local digest registry for hook artifacts (task 3.1).
//!
//! Minimal by Advisory ruling: one admitted registry (`shipped`)
//! rooted at the install sibling directory, serving the one shipped
//! wrapper. No remote fetch, no generalized artifact service.
//!
//! Admission is an exact `(registry, path)` table, not a directory
//! walk: extension input selects among framework-admitted entries,
//! never names files. Opens use `O_NOFOLLOW` (a terminal symlink
//! refuses as `Contract`, never follows), and size plus digest bind
//! to the single opened FD (fstat plus a hard-bounded read —
//! `metadata`-then-`read` by path would race replacement and
//! growth). The trust anchor stays the operator-owned install
//! directory; the digest binds the staged bytes to the request.
//! Staging (verified copy, RO guest mount) rides task 3.2; this
//! module resolves only.

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nix::fcntl::{OFlag, open};
use nix::sys::stat::{Mode, fstat};
use sha2::{Digest, Sha256};

use crate::error::{CistellaError, Result};
use crate::framework::contract::HookArtifact;
use crate::mount::{MountMode, MountTriple};

/// Admitted registry id: the installed sibling directory.
pub const SHIPPED_REGISTRY_ID: &str = "shipped";

/// Wrapper file name inside the shipped registry.
pub const WRAPPER_FILE_NAME: &str = "cistella-landlock-wrap";

/// Known guest path where the isolator stages the wrapper RO
/// pre-initiate; the hook `argv_prefix` names exactly this path.
pub const STAGED_WRAPPER_GUEST_PATH: &str = "/run/cistella/hooks/landlock-wrap";

/// Canonical guest-context probe operation for the Landlock hook.
pub const LANDLOCK_PROBE_OP: &str = "probe_capabilities";

/// Minimum kernel Landlock ABI for the file-access matrix:
/// `TRUNCATE` (bit 14) arrives at ABI 3.
pub const MIN_LANDLOCK_ABI: u32 = 3;

/// Handled file rights for the demonstration matrix: bits 0..=14
/// (`EXECUTE` through `TRUNCATE`). Device, network, and scope
/// controls stay out of scope by design: unhandled rights pass
/// through, documented and reviewable, and the probe refuses any
/// kernel below the TRUNCATE ABI rather than narrowing the mask.
pub const REQUIRED_HANDLED_FS: u64 = (1 << 15) - 1;

/// Ancestor-route rights: execute plus read (`EXECUTE` bit 0,
/// `READ_FILE` bit 2, `READ_DIR` bit 3). Writes are denied by
/// omission; the subtree exception grants the full matrix.
pub const ANCESTOR_RIGHTS: u64 = (1 << 0) | (1 << 2) | (1 << 3);

/// Subtree-route rights: the full demonstration matrix.
pub const SUBTREE_RIGHTS: u64 = REQUIRED_HANDLED_FS;

/// Finite admitted table: the only `(registry, path)` pairs that
/// resolve. Grows only by code change plus review — never by
/// extension input.
const ADMITTED: &[(&str, &str)] = &[(SHIPPED_REGISTRY_ID, WRAPPER_FILE_NAME)];

/// Refusal ceiling for registry reads: a wrapper binary is small;
/// an oversized registry file refuses rather than buffering
/// unboundedly. Enforced on the opened FD (fstat) and again on the
/// read itself (bounded take), so replacement or growth past the
/// check still refuses.
const MAX_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;

/// Staged hook artifact: private per-session verified copy plus
/// its host path. Drop removes the staging directory best-effort,
/// so every conduct exit (success, error, unwind) converges
/// without litter; staged bytes never outlive the session.
#[derive(Debug)]
pub struct StagedHook {
    dir: PathBuf,
    /// Staged host file (bind-mounted RO at the guest path).
    pub host_file: PathBuf,
}

impl Drop for StagedHook {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Stages one hook artifact for a session: resolves (exact
/// admission, digest bind), copies into a private `0700`
/// per-session directory (suffixed by hook order, so parallel
/// hooks never share a file), re-hashes the staged bytes (closing
/// the read-then-stage window), and returns the scope guard plus
/// the RO volume triple for the known guest path.
/// The guard's Drop owns cleanup; callers hold it for the session.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on admission/digest/ceiling
/// refusal or staged re-verification mismatch, and
/// `CistellaError::Io` on staging I/O failure.
pub fn stage_hook_artifact(
    exe_dir: &Path,
    session_tag: &str,
    order: u32,
    artifact: &HookArtifact,
) -> Result<(StagedHook, MountTriple)> {
    let pinned = resolve(
        exe_dir,
        &artifact.source.registry,
        &artifact.source.path,
        &artifact.sha256,
    )?;
    let safe: String = session_tag
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if safe.is_empty() {
        return Err(CistellaError::Contract(
            "staging tag must not be empty".to_string(),
        ));
    }
    let dir = std::env::temp_dir().join(format!("cistella-stage-{safe}-{order}"));
    // Our namespace, our tag: a pre-existing dir is stale crash
    // litter from a dead session, removed before re-staging.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let host_file = dir.join(WRAPPER_FILE_NAME);
    std::fs::write(&host_file, &pinned.bytes)?;
    std::fs::set_permissions(&host_file, std::fs::Permissions::from_mode(0o700))?;
    let staged = std::fs::read(&host_file)?;
    let digest: [u8; 32] = Sha256::digest(&staged).into();
    if digest != pinned.sha256 {
        return Err(CistellaError::Contract(
            "staged artifact digest mismatch".to_string(),
        ));
    }
    let triple = MountTriple {
        host_source: host_file.to_string_lossy().into_owned(),
        container_target: STAGED_WRAPPER_GUEST_PATH.to_string(),
        mode: MountMode::Ro,
    };
    Ok((
        StagedHook {
            dir,
            host_file: host_file.clone(),
        },
        triple,
    ))
}

/// Digest-verified registry bytes plus their coordinates.
#[derive(Debug, Clone)]
pub struct PinnedArtifact {
    /// Verified file bytes (digest matches the request).
    pub bytes: Vec<u8>,
    /// SHA-256 over `bytes`.
    pub sha256: [u8; 32],
    /// Admitted registry id.
    pub registry: String,
    /// Admitted registry-relative path.
    pub path: String,
}

/// Resolves and digest-verifies one hook artifact reference.
///
/// `exe_dir` is the install sibling directory (never PATH).
/// `(registry, path)` must match [`ADMITTED`] exactly; the file
/// opens `O_NOFOLLOW` (symlinks refuse) and size plus digest bind
/// to that single FD.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on unadmitted coordinates,
/// symlink open, digest mismatch, or oversized file, and
/// `CistellaError::Io` on open/read/stat failure.
pub fn resolve(
    exe_dir: &Path,
    registry: &str,
    path: &str,
    expected_sha256: &str,
) -> Result<PinnedArtifact> {
    if !ADMITTED.contains(&(registry, path)) {
        return Err(CistellaError::Contract(
            "registry admission: only the shipped wrapper is admitted".to_string(),
        ));
    }
    let expected = parse_sha256(expected_sha256)?;
    let bytes = read_pinned(&exe_dir.join(path))?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(CistellaError::Contract(
            "artifact exceeds registry read ceiling".to_string(),
        ));
    }
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if digest != expected {
        return Err(CistellaError::Contract(
            "artifact digest mismatch".to_string(),
        ));
    }
    Ok(PinnedArtifact {
        bytes,
        sha256: digest,
        registry: registry.to_string(),
        path: path.to_string(),
    })
}

/// Reads one registry file and returns its lowercase hex SHA-256.
///
/// Sibling-binary reuse: the extension guest advertises the wrapper
/// digest it observes without reimplementing hashing. Unreadable
/// files (including symlinks) surface as `Io`; the caller fails
/// closed.
///
/// # Errors
///
/// Returns `CistellaError::Io` on open/read/stat failure.
pub fn digest_sibling(exe_dir: &Path, file_name: &str) -> Result<String> {
    let bytes = read_pinned(&exe_dir.join(file_name)).map_err(|error| match error {
        CistellaError::Contract(_) => {
            CistellaError::Io(std::io::Error::other("registry file refused"))
        }
        other => other,
    })?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok(hex)
}

/// Opens `O_NOFOLLOW|O_NONBLOCK` and reads with a hard bound.
/// Ownership transfers to `OwnedFd` immediately, so every later
/// `?` closes: an fstat error never leaks. `O_NONBLOCK` keeps an
/// admitted-name FIFO from blocking the open on a writer; the
/// fstat gate then refuses non-regular files (FIFO, directory,
/// device — none is an executable blob) before any read. Size plus
/// digest bind to the single owned FD: the fstat ceiling refuses
/// oversized files before buffering, and the bounded take refuses
/// growth past the check. A terminal symlink refuses with
/// `Contract` (never follows); other open failures surface as `Io`.
fn read_pinned(full: &Path) -> Result<Vec<u8>> {
    let raw = open(
        full,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| {
        if errno == nix::errno::Errno::ELOOP {
            CistellaError::Contract("artifact must not be a symlink".to_string())
        } else {
            CistellaError::Io(std::io::Error::from(errno))
        }
    })?;
    // SAFETY: freshly opened above, owned here, wrapped exactly
    // once (same discipline as the fd-channel accept path).
    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
    let stat =
        fstat(fd.as_raw_fd()).map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
    if stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
        return Err(CistellaError::Contract(
            "admitted artifact must be a regular file".to_string(),
        ));
    }
    let size = stat.st_size;
    if size < 0 || size as u64 > MAX_REGISTRY_BYTES {
        return Err(CistellaError::Contract(
            "artifact exceeds registry read ceiling".to_string(),
        ));
    }
    let file: std::fs::File = fd.into();
    let mut capped = file.take(MAX_REGISTRY_BYTES + 1);
    let mut bytes = Vec::new();
    capped.read_to_end(&mut bytes).map_err(CistellaError::Io)?;
    Ok(bytes)
}

/// Parses 64 lowercase hex into raw digest bytes (same shape the
/// merge gate enforces, so a merged hook always parses here).
fn parse_sha256(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CistellaError::Contract(
            "artifact sha256 must be 64 lowercase hex".to_string(),
        ));
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk)
            .map_err(|_| CistellaError::Contract("artifact sha256 must be hex".to_string()))?;
        out[index] = u8::from_str_radix(text, 16)
            .map_err(|_| CistellaError::Contract("artifact sha256 must be hex".to_string()))?;
    }
    Ok(out)
}
