//! Anchored file reads: the same-FD ownership anchor shared by the
//! registry file and guest binaries.
//!
//! [`begin_anchored`] opens `O_NOFOLLOW` as a regular file and checks
//! seat-or-root ownership plus group/other-writability on the open
//! FD, returning a guard ([`AnchoredOpen`]) that reads with a hard
//! bound from that same FD and re-checks owner/mode after the read.
//! A pathname swap after open proceeds on the pinned original (the
//! open FD names it regardless of the path); an in-place owner/mode
//! change during the window refuses. Only the seat uid or root may
//! own the file and neither group nor other may write it — everyone
//! else is refused by bits (explicit threat-model exception: the
//! registry author IS the seat operator).

use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use nix::fcntl::{OFlag, open};
use nix::sys::stat::{Mode, fstat};

use crate::error::{CistellaError, Result};

/// Opens `path` nofollow as a regular file and checks seat-or-root
/// ownership plus group/other-writability on the open FD, returning
/// a guard that reads with a hard bound from that same FD.
///
/// Returns `Ok(None)` when the path is absent (empty registry —
/// not an error). A pathname swap after open proceeds on the pinned
/// original (the open FD names it regardless of the path);
/// symlink-final and non-regular objects refuse at open. Only the
/// seat uid or root may own the file and neither group nor other may
/// write it — everyone else is refused by bits (only the seat
/// operator or root, both trusted actors, could alter bytes in the
/// window: explicit threat-model exception with rationale — the
/// registry author IS the seat operator).
///
/// The two-phase shape exists so callers hold the pinned FD across
/// phases (resolution through spawn): [`AnchoredOpen::finish`]
/// re-checks owner/mode on the FD after the read, so an in-place
/// change during the window refuses.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on symlink, non-regular,
/// ownership, or mode refusal, and `CistellaError::Io` on
/// open/stat failure.
pub fn begin_anchored(path: &Path, ceiling: u64) -> Result<Option<AnchoredOpen>> {
    let raw = match open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(raw) => raw,
        Err(errno) => {
            if errno == nix::errno::Errno::ENOENT {
                return Ok(None);
            }
            if errno == nix::errno::Errno::ELOOP {
                return Err(CistellaError::Profile(format!(
                    "registry {} must not be a symlink",
                    path.display()
                )));
            }
            return Err(CistellaError::Io(std::io::Error::from(errno)));
        }
    };
    // SAFETY: freshly opened above, owned here, wrapped exactly
    // once (same discipline as the hook-registry read path).
    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
    let before =
        fstat(fd.as_raw_fd()).map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
    check_anchored_stat(&before, path)?;
    Ok(Some(AnchoredOpen {
        file: fd.into(),
        uid: before.st_uid,
        mode: before.st_mode,
        path: path.to_path_buf(),
        ceiling,
    }))
}

/// Open-file guard holding the pinned FD between open-fstat and the
/// bounded read plus post-read fstat (see [`begin_anchored`]).
#[derive(Debug)]
pub struct AnchoredOpen {
    /// Pinned open description (names the verified bytes even if
    /// the path is swapped after open).
    file: std::fs::File,
    /// Owner at open (post-read fstat must match).
    uid: nix::libc::uid_t,
    /// Mode at open (post-read fstat must match).
    mode: nix::libc::mode_t,
    /// Path (diagnostics only — never re-resolved).
    path: PathBuf,
    /// Hard read bound.
    ceiling: u64,
}

/// Bytes read from the pinned FD plus their source path.
#[derive(Debug)]
pub struct AnchoredBytes {
    /// Source path (diagnostics only — the bytes came from the
    /// pinned FD, which may no longer name this path).
    pub path: PathBuf,
    /// Bounded file content.
    pub bytes: Vec<u8>,
}

impl AnchoredOpen {
    /// Reads with the hard bound from the pinned FD, then re-checks
    /// owner/mode on the FD: an in-place change during the window
    /// refuses. Content torn-reads are covered by the trusted-owner
    /// exception — only seat/root could have written, per the
    /// ownership gate at open.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on ceiling or mutation
    /// refusal, and `CistellaError::Io` on read/stat failure.
    pub fn finish(self) -> Result<AnchoredBytes> {
        let AnchoredOpen {
            file,
            uid,
            mode,
            path,
            ceiling,
        } = self;
        let mut capped = (&file).take(ceiling + 1);
        let mut bytes = Vec::new();
        capped.read_to_end(&mut bytes).map_err(CistellaError::Io)?;
        if bytes.len() as u64 > ceiling {
            return Err(CistellaError::Profile(format!(
                "registry {} exceeds read ceiling",
                path.display()
            )));
        }
        let after = fstat(file.as_fd().as_raw_fd())
            .map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
        if after.st_uid != uid || after.st_mode != mode {
            return Err(CistellaError::Profile(format!(
                "registry {} changed owner or mode during read",
                path.display()
            )));
        }
        Ok(AnchoredBytes { path, bytes })
    }

    /// Reads with the hard bound from the pinned FD, re-checks
    /// owner/mode on the FD, requires an execute bit, and compares
    /// the digest against `expected`: the same-FD anchor for guest
    /// binaries. The returned [`PinnedBinary`] holds the verified
    /// open description, so a pathname swap after resolution is
    /// harmless by construction (the FD still names the verified
    /// bytes) and proceeds — but any reopened or swapped object
    /// re-verifies from scratch or refuses.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on ceiling, mutation,
    /// non-executable, or digest-mismatch refusal, and
    /// `CistellaError::Io` on read/stat failure. Diagnostics name
    /// the path, never digest bytes.
    pub fn finish_binary(self, expected: &[u8; 32]) -> Result<PinnedBinary> {
        let AnchoredOpen {
            file,
            uid,
            mode,
            path,
            ceiling,
        } = self;
        let mut capped = (&file).take(ceiling + 1);
        let mut bytes = Vec::new();
        capped.read_to_end(&mut bytes).map_err(CistellaError::Io)?;
        if bytes.len() as u64 > ceiling {
            return Err(CistellaError::Profile(format!(
                "registered guest {} exceeds read ceiling",
                path.display()
            )));
        }
        let after = fstat(file.as_fd().as_raw_fd())
            .map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
        if after.st_uid != uid || after.st_mode != mode {
            return Err(CistellaError::Profile(format!(
                "registered guest {} changed owner or mode during read",
                path.display()
            )));
        }
        if after.st_mode & 0o111 == 0 {
            return Err(CistellaError::Profile(format!(
                "registered guest binary is not executable: {}",
                path.display()
            )));
        }
        let digest: [u8; 32] = sha256_bytes(&bytes);
        if &digest != expected {
            return Err(CistellaError::Profile(format!(
                "registered guest digest mismatch: {}",
                path.display()
            )));
        }
        Ok(PinnedBinary { file, digest })
    }
}

/// Verified guest binary: the digest-checked open file description
/// whose object is exec'd (spawn execs `/proc/self/fd/N`, so
/// identity and execution share one open description).
#[derive(Debug)]
pub struct PinnedBinary {
    /// Open verified executable object (held through spawn).
    pub file: std::fs::File,
    /// SHA-256 over the verified bytes (equals the registry hash).
    pub digest: [u8; 32],
}

/// SHA-256 over one byte slice.
pub(crate) fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// Checks one fstat result: regular file, seat-or-root owner, no
/// group/other write bits.
fn check_anchored_stat(stat: &nix::sys::stat::FileStat, path: &Path) -> Result<()> {
    if stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
        return Err(CistellaError::Profile(format!(
            "registry {} must be a regular file",
            path.display()
        )));
    }
    // SAFETY: getuid is inherently safe (same call as the
    // fd-channel accept path).
    let seat = unsafe { nix::libc::getuid() };
    if stat.st_uid != seat && stat.st_uid != 0 {
        return Err(CistellaError::Profile(format!(
            "registry {} must be seat- or root-owned",
            path.display()
        )));
    }
    if stat.st_mode & 0o022 != 0 {
        return Err(CistellaError::Profile(format!(
            "registry {} must not be group/world-writable",
            path.display()
        )));
    }
    Ok(())
}
