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

    /// Parses a mode flag (`ro`/`rw`).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Mount` on anything else.
    pub fn parse_flag(s: &str) -> Result<Self> {
        match s {
            "ro" => Ok(Self::Ro),
            "rw" => Ok(Self::Rw),
            other => Err(CistellaError::Mount(format!(
                "mode must be ro or rw: {other}"
            ))),
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

/// Parses a CLI `--mount <host>:<target>:<mode>` item into a triple.
///
/// Structural parse only (three colon-separated parts, non-empty paths,
/// valid mode); full validation happens in [`validate_mounts`].
///
/// # Errors
///
/// Returns `CistellaError::Mount` on wrong arity, empty paths, or a bad
/// mode. Host paths containing `:` cannot be expressed; document, do not
/// work around.
///
/// # Examples
///
/// ```
/// # use cistella::mount::{MountMode, parse_mount_triple};
/// let t = parse_mount_triple("/data:/data:ro").unwrap();
/// assert_eq!(t.host_source, "/data");
/// assert_eq!(t.mode, MountMode::Ro);
/// ```
pub fn parse_mount_triple(s: &str) -> Result<MountTriple> {
    let mut parts = s.split(':');
    let triple = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(host), Some(target), Some(mode), None) => (host, target, mode),
        _ => {
            return Err(CistellaError::Mount(format!(
                "--mount must be <host>:<target>:<mode>: {s}"
            )));
        }
    };
    if triple.0.is_empty() || triple.1.is_empty() {
        return Err(CistellaError::Mount(format!(
            "--mount host and target must be non-empty: {s}"
        )));
    }
    Ok(MountTriple {
        host_source: triple.0.to_string(),
        container_target: triple.1.to_string(),
        mode: MountMode::parse_flag(triple.2)?,
    })
}

/// Parses `--session-directory <host>[:<container>]` into host and target.
///
/// The container side defaults to `/work` when omitted.
///
/// # Errors
///
/// Returns `CistellaError::Mount` on an empty host, an empty container
/// side, or a non-absolute container target. Host absoluteness is checked
/// by the caller (`canonical_directory`).
///
/// # Examples
///
/// ```
/// # use cistella::mount::parse_session_directory;
/// assert_eq!(
///     parse_session_directory("/repo").unwrap(),
///     ("/repo".to_string(), "/work".to_string())
/// );
/// assert_eq!(
///     parse_session_directory("/repo:/repo").unwrap(),
///     ("/repo".to_string(), "/repo".to_string())
/// );
/// ```
pub fn parse_session_directory(s: &str) -> Result<(String, String)> {
    let (host, target) = match s.split_once(':') {
        Some((host, target)) => (host, Some(target)),
        None => (s, None),
    };
    if host.is_empty() {
        return Err(CistellaError::Mount(format!(
            "session directory host must be non-empty: {s}"
        )));
    }
    let target = target.unwrap_or("/work");
    if target.is_empty() {
        return Err(CistellaError::Mount(format!(
            "session directory container target must be non-empty: {s}"
        )));
    }
    if !target.starts_with('/') {
        return Err(CistellaError::Mount(format!(
            "session directory container target must be absolute: {s}"
        )));
    }
    Ok((host.to_string(), target.to_string()))
}

/// Unions CLI triples over profile triples for one conduct invocation.
///
/// Exact canonical-target matches override (CLI wins); duplicate CLI
/// targets, CLI triples on the worktree target, and ancestor/descendant
/// CLI/profile overlap outside the read-only-ancestor rule are typed
/// errors. The merged list still requires [`validate_mounts`].
///
/// # Errors
///
/// Returns `CistellaError::Mount` on any of the above collisions.
///
/// # Examples
///
/// ```
/// # use cistella::mount::{MountMode, MountTriple, merge_cli_mounts};
/// # fn triple(h: &str, t: &str, m: MountMode) -> MountTriple {
/// #     MountTriple { host_source: h.into(), container_target: t.into(), mode: m }
/// # }
/// let merged = merge_cli_mounts(
///     &[triple("/a", "/data", MountMode::Ro)],
///     &[triple("/b", "/data", MountMode::Rw)],
///     "/work",
/// )
/// .unwrap();
/// assert_eq!(merged.len(), 1);
/// assert_eq!(merged[0].host_source, "/b");
/// ```
pub fn merge_cli_mounts(
    profile: &[MountTriple],
    cli: &[MountTriple],
    worktree_target: &str,
) -> Result<Vec<MountTriple>> {
    let canon_worktree = canonicalize_container_target(worktree_target);
    let mut seen_cli: Vec<String> = Vec::new();
    for t in cli {
        let canon = canonicalize_container_target(&t.container_target);
        if seen_cli.contains(&canon) {
            return Err(CistellaError::Mount(format!(
                "duplicate --mount target: {}",
                t.container_target
            )));
        }
        seen_cli.push(canon.clone());
        if canon == canon_worktree {
            return Err(CistellaError::Mount(format!(
                "--mount target {} equals the worktree target; use --session-directory",
                t.container_target
            )));
        }
    }
    let mut merged: Vec<MountTriple> = profile.to_vec();
    for t in cli {
        let canon = canonicalize_container_target(&t.container_target);
        if let Some(pos) = merged
            .iter()
            .position(|p| canonicalize_container_target(&p.container_target) == canon)
        {
            merged[pos] = t.clone();
            continue;
        }
        for p in profile {
            let canon_profile = canonicalize_container_target(&p.container_target);
            if canon_profile != canon
                && (is_ancestor_or_equal(&canon_profile, &canon)
                    || is_ancestor_or_equal(&canon, &canon_profile))
                && !overlap_allowed(&canon_profile, p.mode, &canon, t.mode)
            {
                return Err(CistellaError::Mount(format!(
                    "--mount {} partially overlaps profile mount {}",
                    t.container_target, p.container_target
                )));
            }
        }
        merged.push(t.clone());
    }
    Ok(merged)
}

/// Whether an ancestor/descendant triple pair may stack: the ancestor
/// triple is read-only (deepest mount wins either way).
fn overlap_allowed(canon_a: &str, mode_a: MountMode, canon_b: &str, mode_b: MountMode) -> bool {
    if canon_a == canon_b {
        return false;
    }
    if is_ancestor_or_equal(canon_a, canon_b) {
        mode_a == MountMode::Ro
    } else if is_ancestor_or_equal(canon_b, canon_a) {
        mode_b == MountMode::Ro
    } else {
        false
    }
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

    // Pairwise overlap among triples (after canonicalization): exact
    // duplicates always fail; strict ancestor/descendant pairs stack only
    // over a read-only ancestor (deepest mount wins).
    let mut canons: Vec<(&MountTriple, String)> = triples
        .iter()
        .map(|t| (t, canonicalize_container_target(&t.container_target)))
        .collect();
    // Sort by target for deterministic checks.
    canons.sort_by(|a, b| a.1.cmp(&b.1));
    for i in 0..canons.len() {
        for j in (i + 1)..canons.len() {
            let (ta, ca) = &canons[i];
            let (tb, cb) = &canons[j];
            if ca == cb {
                return Err(CistellaError::Mount(format!(
                    "duplicate mount target: {}",
                    ta.container_target
                )));
            }
            if (is_ancestor_or_equal(ca, cb) || is_ancestor_or_equal(cb, ca))
                && !overlap_allowed(ca, ta.mode, cb, tb.mode)
            {
                return Err(CistellaError::Mount(format!(
                    "overlapping mounts: {} and {}",
                    ta.container_target, tb.container_target
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
    // Same stacking rule as container targets: strict host-side overlap is
    // allowed only over a read-only ancestor; identical host sources stay
    // an error (ambiguous intent, not stacking).
    let mut canon_hosts: Vec<(&MountTriple, PathBuf)> = triples
        .iter()
        .map(|t| (t, canonicalize_host_source(&t.host_source)))
        .collect();
    canon_hosts.sort_by(|a, b| a.1.cmp(&b.1));
    for i in 0..canon_hosts.len() {
        for j in (i + 1)..canon_hosts.len() {
            let (ta, ha) = &canon_hosts[i];
            let (tb, hb) = &canon_hosts[j];
            if ha == hb {
                return Err(CistellaError::Mount(format!(
                    "overlapping host_sources after canonicalize: {} and {}",
                    ha.display(),
                    hb.display()
                )));
            }
            if (ha.starts_with(hb) || hb.starts_with(ha))
                && !overlap_allowed_host(ta.mode, tb.mode, ha, hb)
            {
                return Err(CistellaError::Mount(format!(
                    "overlapping host_sources after canonicalize: {} and {}",
                    ha.display(),
                    hb.display()
                )));
            }
        }
    }

    Ok(())
}

/// Whether a strict host-side ancestor/descendant pair may stack: the
/// ancestor triple is read-only. PathBuf `starts_with` is
/// component-wise, so no string-prefix false positives.
fn overlap_allowed_host(
    mode_a: MountMode,
    mode_b: MountMode,
    host_a: &Path,
    host_b: &Path,
) -> bool {
    if host_a == host_b {
        return false;
    }
    if host_a.starts_with(host_b) {
        mode_b == MountMode::Ro
    } else if host_b.starts_with(host_a) {
        mode_a == MountMode::Ro
    } else {
        false
    }
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
