//! Allowlist-only mount triples with two-tier topology.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::{CistellaError, Result};

/// Mount mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    /// Read-only.
    Ro,
    /// Read-write.
    Rw,
}

impl MountMode {
    #[must_use]
    pub fn as_flag(self) -> &'static str {
        match self {
            Self::Ro => "ro",
            Self::Rw => "rw",
        }
    }
}

/// Single allowlist triple `(host-source, container-target, mode)`.
#[derive(Debug, Clone, Deserialize)]
pub struct MountTriple {
    /// Host path.
    pub host_source: String,
    /// Container path.
    pub container_target: String,
    /// Mode.
    pub mode: MountMode,
}

/// Sensitive container roots that must not be used as target.
const SENSITIVE_ROOTS: &[&str] = &["/", "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64"];

/// Validates the allowlist triples against the two-tier topology.
///
/// # Errors
///
/// Returns `CistellaError::Mount` on overlap, sensitive target, or
/// malformed paths.
pub fn validate_mounts(triples: &[MountTriple], container_home: &str) -> Result<()> {
    for t in triples {
        if t.host_source.contains('\n')
            || t.host_source.contains('\r')
            || t.host_source.contains('=')
        {
            return Err(CistellaError::Mount(format!(
                "host_source must not contain control/'=': {}",
                t.host_source
            )));
        }
        if t.container_target.contains('\n')
            || t.container_target.contains('\r')
            || t.container_target.contains('=')
        {
            return Err(CistellaError::Mount(format!(
                "container_target must not contain control/'=': {}",
                t.container_target
            )));
        }
        if !t.host_source.starts_with('/') {
            return Err(CistellaError::Mount(format!(
                "host_source must be absolute: {}",
                t.host_source
            )));
        }
        if !t.container_target.starts_with('/') {
            return Err(CistellaError::Mount(format!(
                "container_target must be absolute: {}",
                t.container_target
            )));
        }
        let canon_target = canonicalize_container_target(&t.container_target);
        for root in SENSITIVE_ROOTS {
            if canon_target == *root || canon_target.starts_with(&format!("{root}/")) {
                // Allow nesting under session-home even if session-home is under
                // a sensitive prefix? No — session-home is outside triples.
                // Reject triples at or above sensitive roots.
                if is_ancestor_or_equal(root, &canon_target) {
                    return Err(CistellaError::Mount(format!(
                        "container_target at or above sensitive root {}: {}",
                        root, t.container_target
                    )));
                }
            }
        }
        // Also reject bare "/" even if not in list (covered above).
        if canon_target == "/" {
            return Err(CistellaError::Mount(
                "container_target must not be /".to_string(),
            ));
        }
    }

    // Pairwise disjointness among triples (after canonicalization).
    let mut canon_targets: Vec<String> = triples
        .iter()
        .map(|t| canonicalize_container_target(&t.container_target))
        .collect();
    // Sort by length for deterministic checks.
    canon_targets.sort();
    for i in 0..canon_targets.len() {
        for j in (i + 1)..canon_targets.len() {
            if is_ancestor_or_equal(&canon_targets[i], &canon_targets[j])
                || is_ancestor_or_equal(&canon_targets[j], &canon_targets[i])
            {
                return Err(CistellaError::Mount(format!(
                    "overlapping mounts: {} and {}",
                    canon_targets[i], canon_targets[j]
                )));
            }
        }
    }

    // Reject triple that would shadow session-home tmpfs (mount order puts triples after home).
    let canon_home = canonicalize_container_target(container_home);
    for t in triples {
        let canon_target = canonicalize_container_target(&t.container_target);
        if is_ancestor_or_equal(&canon_target, &canon_home) {
            return Err(CistellaError::Mount(format!(
                "container_target {} shadows session-home {}",
                t.container_target, container_home
            )));
        }
    }

    // Triples may nest under session-home; that is allowed and does not
    // count as overlap with the session-home primitive (which is not a
    // triple). No further check.

    // Canonicalize both sides host-side via longest existing prefix to
    // detect symlink aliasing; spec requires canonicalize before validation.
    let mut canon_hosts: Vec<PathBuf> = triples
        .iter()
        .map(|t| canonicalize_host_source(&t.host_source))
        .collect();
    canon_hosts.sort();
    for i in 0..canon_hosts.len() {
        for j in (i + 1)..canon_hosts.len() {
            if canon_hosts[i] == canon_hosts[j]
                || canon_hosts[i].starts_with(&canon_hosts[j])
                || canon_hosts[j].starts_with(&canon_hosts[i])
            {
                return Err(CistellaError::Mount(format!(
                    "overlapping host_sources after canonicalize: {} and {}",
                    canon_hosts[i].display(),
                    canon_hosts[j].display()
                )));
            }
        }
    }

    Ok(())
}

/// Canonicalizes a container target: cleans `.`, `..`, duplicate slashes
/// without touching the filesystem.
pub(crate) fn canonicalize_container_target(path: &str) -> String {
    let mut out = PathBuf::new();
    for comp in Path::new(path).components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    let s = out.to_string_lossy().to_string();
    if s.is_empty() { "/".to_string() } else { s }
}

fn is_ancestor_or_equal(ancestor: &str, descendant: &str) -> bool {
    if ancestor == descendant {
        return true;
    }
    let anc = ancestor.trim_end_matches('/');
    let desc = descendant.trim_end_matches('/');
    if anc.is_empty() {
        return true;
    }
    desc == anc || desc.starts_with(&format!("{anc}/"))
}

/// Returns podman volume args for the mounts in required ordering:
///
/// session-home first (tmpfs or scratch), then triples sorted by path depth
/// (shallowest first) so parents are mounted before children.
///
#[must_use]
pub fn podman_volume_args(
    triples: &[MountTriple],
    container_home: &str,
    session_home_source: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(src) = session_home_source {
        // tmpfs or bind for HOME
        if src == "tmpfs" {
            args.push("--tmpfs".to_string());
            args.push(container_home.to_string());
        } else {
            args.push("--volume".to_string());
            args.push(format!("{src}:{container_home}:rw"));
        }
    } else {
        args.push("--tmpfs".to_string());
        args.push(container_home.to_string());
    }
    let mut sorted = triples.to_vec();
    sorted.sort_by_key(|t| t.container_target.matches('/').count());
    for t in sorted {
        args.push("--volume".to_string());
        args.push(format!(
            "{}:{}:{}",
            t.host_source,
            canonicalize_container_target(&t.container_target),
            t.mode.as_flag()
        ));
    }
    args
}

/// Canonicalizes host path via longest existing prefix, mirroring
/// `dispositor::assert_disjoint_roots` precedent.
#[must_use]
pub fn canonicalize_host_source(path: &str) -> PathBuf {
    let p = Path::new(path);
    // Walk up until an existing ancestor is found, then join remainder.
    let mut cur = p;
    let mut remainder = Vec::new();
    loop {
        if cur.exists()
            && let Ok(canon) = cur.canonicalize()
        {
            let mut out = canon;
            for comp in remainder.iter().rev() {
                out.push(comp);
            }
            return out;
        }
        if let Some(parent) = cur.parent() {
            if let Some(name) = cur.file_name() {
                remainder.push(name.to_owned());
            }
            if parent == cur {
                break;
            }
            cur = parent;
        } else {
            break;
        }
    }
    PathBuf::from(path)
}
