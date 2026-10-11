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
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use nix::fcntl::{OFlag, open, openat};
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
    anchor_from_fd(fd, path.to_path_buf(), ceiling).map(Some)
}

/// Opens `name` relative to an already-verified directory FD and
/// anchors it identically: nofollow, regular-file, seat-or-root
/// ownership, no group/other write bits, all on the open
/// description. Single-component names only (a `/` refuses —
/// callers pass hash slots and generated temp names, never paths).
/// Directory-FD-relative creation/open/rename is what makes the
/// cache guard structural: no path operation can be substituted
/// between check and use because there is no path operation.
///
/// Returns `Ok(None)` when the name is absent under the directory.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on symlink, non-regular,
/// ownership, or mode refusal, and `CistellaError::Io` on
/// open/stat failure.
pub fn openat_anchored(
    dirfd: RawFd,
    name: &str,
    ceiling: u64,
    display_dir: &Path,
) -> Result<Option<AnchoredOpen>> {
    if name.contains('/') {
        return Err(CistellaError::Profile(
            "anchored relative name must be single-component".to_string(),
        ));
    }
    let raw = match openat(
        Some(dirfd),
        Path::new(name),
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
                    display_dir.join(name).display()
                )));
            }
            return Err(CistellaError::Io(std::io::Error::from(errno)));
        }
    };
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
    anchor_from_fd(fd, display_dir.join(name), ceiling).map(Some)
}

/// Fstats one freshly opened FD and wraps the ownership-checked
/// guard: the shared tail of every anchored open (path or
/// directory-relative).
fn anchor_from_fd(fd: OwnedFd, path: PathBuf, ceiling: u64) -> Result<AnchoredOpen> {
    let before =
        fstat(fd.as_raw_fd()).map_err(|errno| CistellaError::Io(std::io::Error::from(errno)))?;
    check_anchored_stat(&before, &path)?;
    Ok(AnchoredOpen {
        file: fd.into(),
        uid: before.st_uid,
        mode: before.st_mode,
        path,
        ceiling,
    })
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

/// Read ceiling for driver state files (leases, cleanup records):
/// small text, never binaries.
pub const MAX_STATE_BYTES: u64 = 64 * 1024;

/// Sequence for staging-temp uniqueness within the process
/// (uniqueness across processes comes from the pid).
static STATE_TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Open verified private directory: the single enforcement point
/// for driver state directories (fetch cache, cleanup/lease
/// state). Creates the leaf when absent, then opens it nofollow
/// as a directory, checks seat-or-root ownership on the open FD,
/// and hardens the mode to 0700 on the FD (never by path, so a
/// concurrent substitution cannot redirect the chmod). A symlink
/// at the path refuses instead of having its target created
/// through, chmodded, or operated in.
///
/// Errors are plain strings so callers wrap them in their own
/// error class (registry, lock, and cleanup surfaces differ).
///
/// # Errors
///
/// Returns `Err` on symlink, non-directory, ownership, or I/O
/// refusal with a message naming the path.
pub fn open_state_dir(path: &Path) -> std::result::Result<AnchoredDir, String> {
    // 0700-at-creation (DirBuilder mode is umask-masked, but
    // 0700 has no group/other bits for any umask to clear), then
    // fchmod hardening below covers pre-existing directories.
    // A symlink at the path refuses at the nofollow open — never
    // created through.
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!("state dir {}: {error}", path.display()));
            }
        }
    }
    let raw = nix::fcntl::open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| {
        if errno == nix::errno::Errno::ELOOP {
            format!("state dir {} must not be a symlink", path.display())
        } else if errno == nix::errno::Errno::ENOTDIR {
            // O_NOFOLLOW|O_DIRECTORY reports a symlink-to-dir as
            // ENOTDIR rather than ELOOP: disambiguate for the
            // diagnostic only (the nofollow open already refused —
            // no trust decision rides this second look).
            let symlink = std::fs::symlink_metadata(path)
                .map(|stat| stat.file_type().is_symlink())
                .unwrap_or(false);
            if symlink {
                format!("state dir {} must not be a symlink", path.display())
            } else {
                format!("state dir {} must be a directory", path.display())
            }
        } else {
            format!("state dir {}: {errno}", path.display())
        }
    })?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
    let stat = nix::sys::stat::fstat(fd.as_raw_fd())
        .map_err(|errno| format!("state dir {}: {errno}", path.display()))?;
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    if stat.st_uid != seat && stat.st_uid != 0 {
        return Err(format!(
            "state dir {} must be seat- or root-owned",
            path.display()
        ));
    }
    nix::sys::stat::fchmod(fd.as_raw_fd(), Mode::from_bits_truncate(0o700))
        .map_err(|errno| format!("state dir {}: {errno}", path.display()))?;
    Ok(AnchoredDir {
        fd,
        path: path.to_path_buf(),
    })
}

/// Open verified private directory: every file operation inside
/// runs directory-FD-relative (openat/renameat/unlinkat), so path
/// substitution between check and use is structural, not checked.
#[derive(Debug)]
pub struct AnchoredDir {
    /// Open verified directory (all operations relative).
    fd: OwnedFd,
    /// Path (diagnostics and display only — never re-resolved).
    path: PathBuf,
}

impl AnchoredDir {
    /// Raw directory FD for dirfd-relative operations.
    #[must_use]
    pub fn dirfd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Directory path (diagnostics and display paths only —
    /// never re-resolved for trust).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates one file O_EXCL inside the directory (symlinks
    /// refuse via O_NOFOLLOW; pre-existing names refuse via
    /// O_EXCL), hardens it to 0600 on the open FD, and returns
    /// the open description for writing. Single-component names
    /// only.
    ///
    /// # Errors
    ///
    /// Returns `Err` on symlink, existing name, or I/O refusal.
    pub fn create_file(&self, name: &str) -> std::result::Result<std::fs::File, String> {
        if name.contains('/') {
            return Err("anchored relative name must be single-component".to_string());
        }
        let raw = nix::fcntl::openat(
            Some(self.dirfd()),
            Path::new(name),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|errno| {
            if errno == nix::errno::Errno::ELOOP {
                format!("state file {name} must not be a symlink")
            } else if errno == nix::errno::Errno::EEXIST {
                format!("state file {name} already exists")
            } else {
                format!("state file {name}: {errno}")
            }
        })?;
        // SAFETY: freshly opened above, owned here, wrapped exactly once.
        let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
        nix::sys::stat::fchmod(fd.as_raw_fd(), Mode::from_bits_truncate(0o600))
            .map_err(|errno| format!("state file {name}: {errno}"))?;
        Ok(std::fs::File::from(fd))
    }

    /// Reads one file bounded from its nofollow-opened FD, with
    /// seat-or-root ownership checked on that same FD. `None`
    /// when absent; symlinks refuse (never followed).
    ///
    /// # Errors
    ///
    /// Returns `Err` on symlink, ownership, ceiling, or I/O refusal.
    pub fn read_file(
        &self,
        name: &str,
        ceiling: u64,
    ) -> std::result::Result<Option<Vec<u8>>, String> {
        if name.contains('/') {
            return Err("anchored relative name must be single-component".to_string());
        }
        let Some(open) = openat_anchored(self.dirfd(), name, ceiling, &self.path)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        open.finish()
            .map(|anchored| anchored.bytes)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// Atomically replaces one file: unique O_EXCL temp plus
    /// renameat inside the directory (rename never traverses a
    /// planted final symlink — it replaces the link). Single-
    /// component names only.
    ///
    /// # Errors
    ///
    /// Returns `Err` on staging or I/O refusal.
    pub fn replace_file(&self, name: &str, bytes: &[u8]) -> std::result::Result<(), String> {
        if name.contains('/') {
            return Err("anchored relative name must be single-component".to_string());
        }
        let tmp = format!(
            ".tmp-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0),
            STATE_TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut staged = self.create_file(&tmp)?;
        use std::io::Write;
        if let Err(error) = staged.write_all(bytes) {
            drop(staged);
            self.remove_file(&tmp);
            return Err(format!("state file {tmp}: {error}"));
        }
        drop(staged);
        nix::fcntl::renameat(
            Some(self.dirfd()),
            Path::new(&tmp),
            Some(self.dirfd()),
            Path::new(name),
        )
        .map_err(|e| format!("state file {name}: {e}"))?;
        Ok(())
    }

    /// Best-effort unlink of one name inside the directory (never
    /// follows: unlinkat removes the link itself).
    pub fn remove_file(&self, name: &str) {
        if name.contains('/') {
            return;
        }
        let _ = nix::unistd::unlinkat(
            Some(self.dirfd()),
            Path::new(name),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        );
    }
}
