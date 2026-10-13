//! Pre-create initialization over the merged mount topology:
//! the final joint gate (validate, preflight, scoped seeding)
//! plus the anchored nested-mountpoint walk. Separated from
//! `mount` (allowlist shapes) so each file stays under the
//! line gate; the conduct call site names this module.

use std::path::{Component, Path, PathBuf};

use crate::error::{CistellaError, Result};
use crate::mount::{
    MountTriple, canonicalize_container_target, canonicalize_host_source, is_ancestor_or_equal,
    nested_ro_preflight, validate_mounts,
};

/// Pre-creates nested bind mountpoints inside their parent's host
/// source before container start. Podman auto-creates a missing
/// mountpoint as container-root (host uid 100000 rootless); a
/// stranger-owned dir inside a seat-owned tree later refuses
/// traversal-trusted teardown (a nested bind whose mountpoint
/// podman auto-creates as container-root). Seat-owned
/// pre-created dirs are reused as-is.
/// Strict nesting only; top-level mounts resolve into the image
/// fs and need nothing. Scoped to declared cleanup roots — the
/// parent source must sit at/under a root, so lifetime teardown
/// (or gc) removes anything created: no new residue class, no
/// writes into user source trees. Other nesting skips (podman
/// legacy behavior, unchanged). Non-absolute sources and missing
/// parents skip (validated elsewhere). Creation is an anchored
/// nofollow walk (a guest plants symlinks pre-gate — never
/// follow one out of the admitted source); pre-existing chain
/// dirs must be seat/root-owned. Runs pre-create.
///
/// # Errors
///
/// Returns `CistellaError::Mount` on refusal or creation failure.
pub fn precreate_nested_mountpoints(
    triples: &[MountTriple],
    cleanup_roots: &[String],
) -> Result<()> {
    let roots: Vec<PathBuf> = cleanup_roots
        .iter()
        .map(|root| canonicalize_host_source(root))
        .collect();
    for triple in triples {
        let target = canonicalize_container_target(&triple.container_target);
        let mut parent: Option<&MountTriple> = None;
        let mut parent_len = 0;
        for candidate in triples {
            let canon = canonicalize_container_target(&candidate.container_target);
            if canon.len() >= target.len() || !is_ancestor_or_equal(&canon, &target) {
                continue;
            }
            if parent.is_none() || canon.len() > parent_len {
                parent_len = canon.len();
                parent = Some(candidate);
            }
        }
        let Some(host) = parent else { continue };
        if !host.host_source.starts_with('/') {
            continue;
        }
        let source = canonicalize_host_source(&host.host_source);
        if !source.is_dir() || !roots.iter().any(|root| source.starts_with(root)) {
            continue;
        }
        let remainder = target
            .strip_prefix(
                canonicalize_container_target(&host.container_target).trim_end_matches('/'),
            )
            .unwrap_or("")
            .trim_start_matches('/');
        mkdir_anchored(&source, remainder, &target)?;
    }
    Ok(())
}

/// Creates one nested mountpoint through an anchored nofollow
/// walk from the admitted parent source: each component is
/// mkdirat-created (0700, seat-owned by construction) or opened
/// nofollow, descending FD-relative. A symlink anywhere refuses
/// (ELOOP names it — planted pre-gate, never followed out);
/// pre-existing non-dirs (ENOTDIR) and stranger-owned dirs
/// refuse likewise. Per-component syscalls, no check-then-act.
fn mkdir_anchored(parent: &Path, remainder: &str, display: &str) -> Result<()> {
    use nix::fcntl::OFlag;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    let refuse = |why: &str| CistellaError::Mount(format!("nested mountpoint {display}: {why}"));
    let raw = nix::fcntl::open(
        parent,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| refuse(&format!("open parent: {e}")))?;
    // SAFETY: freshly opened above, owned here, wrapped exactly once.
    let mut dir: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
    for component in Path::new(remainder).components() {
        let Component::Normal(name) = component else {
            return Err(refuse("bad component"));
        };
        let cname = std::ffi::CString::new(name.as_bytes()).map_err(|_| refuse("bad name"))?;
        match nix::sys::stat::mkdirat(
            Some(dir.as_raw_fd()),
            cname.as_c_str(),
            nix::sys::stat::Mode::S_IRWXU,
        ) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
            Err(e) => return Err(refuse(&format!("create {}: {e}", name.to_string_lossy()))),
        }
        let raw = nix::fcntl::openat(
            Some(dir.as_raw_fd()),
            cname.as_c_str(),
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|e| refuse(&format!("open {}: {e}", name.to_string_lossy())))?;
        // SAFETY: freshly opened above, owned here, wrapped exactly once.
        let opened: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
        let stat = nix::sys::stat::fstat(opened.as_raw_fd())
            .map_err(|e| refuse(&format!("fstat: {e}")))?;
        if stat.st_uid != seat && stat.st_uid != 0 {
            return Err(refuse("stranger-owned chain"));
        }
        dir = opened;
    }
    Ok(())
}

/// Final joint topology gate over the merged set (extension mounts
/// plus credential volumes): validate, nested-RO preflight, then
/// nested-mountpoint pre-create scoped to declared cleanup roots.
/// Callers run it pre-create, so a refusal leaves no unit or
/// container residue (created mountpoints persist as attributable
/// litter under their cleanup root until teardown or gc).
///
/// # Errors
///
/// Returns `CistellaError::Mount` on the first gate failure.
pub fn gate_final_topology(
    triples: &[MountTriple],
    container_home: &str,
    cleanup_roots: &[String],
) -> Result<()> {
    validate_mounts(triples, container_home)?;
    nested_ro_preflight(triples)?;
    precreate_nested_mountpoints(triples, cleanup_roots)
}
