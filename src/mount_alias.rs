//! Dentry-identity hardening for the graft-alias preflight.
//!
//! The path-based guard ([`crate::mount::graft_alias_preflight`])
//! compares canonical spellings, and `canonicalize` resolves
//! symlinks — but not bind mounts. A read-write graft of
//! ancestor-tree content through a bind-mount path outside the
//! ancestor passes the spelling check while sharing dentries
//! with read-only-covered content; the graft's FULL carveout
//! then admits writes through every alias (Landlock rules are
//! dentry-based and mount-agnostic). The reverse evasion is a
//! graft path-wise inside the subtree whose dentry sits outside
//! it (a bind escape): the path guard waves it through as
//! already-FULL while the carveout grants undeclared content.
//!
//! This leg compares dentry identity instead of spellings, in
//! two strengths: exact `(st_dev, st_ino)` equality catches a
//! directory bound twice under different paths; filesystem-
//! relative containment through the host mount table catches a
//! subdirectory bound elsewhere (same filesystem, fs-path under
//! a sensitive root, host path outside it). Sensitive roots are
//! content the ruleset leaves non-FULL — the ancestor tree and
//! every read-only directory declaration — minus content
//! already FULL (the subtree and every read-write graft source:
//! FULL-on-FULL aliasing is declared intent twice, not a
//! contradiction — but a second read-write bind never
//! precedes the sensitive check, so paired aliases of one
//! read-only dentry refuse on every spelling). Missing paths
//! contribute no identity (no dentry, nothing to alias —
//! matches the path guard's not-yet-existing skip; a
//! wrong-kind materialization fails loudly at apply). An
//! absent, empty, or non-covering mount table refuses
//! candidates needing containment proof rather than
//! accepting unknown: the table is a host interface, and an
//! unprovable disjointness is a typed pre-create refusal,
//! not a pass. (Graft-free topologies return before any
//! table read.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{CistellaError, Result};
use crate::mount::{MountMode, MountTriple, canonicalize_host_source};

/// Dentry identity of one directory: owning device plus inode
/// number. Equal pairs on one host are the same directory,
/// whatever the path spelling (bind mounts included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DentryId {
    /// `st_dev`: the filesystem owning the dentry.
    pub dev: u64,
    /// `st_ino`: the inode number on that filesystem.
    pub ino: u64,
}

/// Identity of one existing host directory (`None` for missing
/// paths and non-directories: files carry no Landlock rules
/// and sockets ride the credential surface, so neither can
/// alias directory grants).
#[must_use]
pub fn dentry_id(path: &Path) -> Option<DentryId> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    Some(DentryId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// One `/proc/self/mountinfo` entry: where a filesystem (or
/// bind of it) is attached, which device backs it, and which
/// filesystem-relative root it exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MountEntry {
    /// Host path the entry is mounted on.
    mount_point: PathBuf,
    /// Filesystem-relative path exposed at the mount point (a
    /// bind mount's source, in filesystem namespace).
    fs_root: PathBuf,
    /// `(major, minor)` device number of the backing filesystem.
    dev: (u64, u64),
}

/// Parsed host mount table: longest-prefix mount resolution
/// with filesystem-relative paths. Same device plus
/// containment in filesystem namespace proves one path's
/// dentries sit under another's — across bind mounts, where
/// spelling comparison goes blind.
#[derive(Debug, Clone, Default)]
pub struct MountTable {
    entries: Vec<MountEntry>,
}

impl MountTable {
    /// Parses `/proc/self/mountinfo` text (see
    /// [`MountTable::read_host`] for the live source).
    /// Strict: any non-blank line that does not parse
    /// rejects the whole table (`None`) — a silently
    /// dropped bind-mount line would fall back to a root
    /// prefix and map an alias to a fake filesystem-
    /// relative path, admitting it. A mangled table proves
    /// nothing; callers refuse rather than compare blind.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut entries = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            entries.push(parse_mountinfo_line(line)?);
        }
        Some(Self { entries })
    }

    /// Reads the live host mount table. `None` when
    /// `/proc/self/mountinfo` is missing, unreadable, or
    /// malformed (non-Linux or mangled seats): callers
    /// refuse candidates needing containment proof rather
    /// than invent coverage.
    #[must_use]
    pub fn read_host() -> Option<Self> {
        let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        Self::parse(&text)
    }

    /// Whether the table parsed zero entries: an empty
    /// table resolves nothing and proves nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolves a canonical host path to its backing device
    /// plus filesystem-relative path: longest mount-point
    /// prefix wins (later entries break ties, matching
    /// overmount order), remainder joins the entry's fs root.
    /// `None` only for an empty table (no root mount parsed).
    #[must_use]
    pub fn filesystem_path(&self, canonical: &Path) -> Option<((u64, u64), PathBuf)> {
        let mut best: Option<&MountEntry> = None;
        for entry in &self.entries {
            if !canonical.starts_with(&entry.mount_point) {
                continue;
            }
            let longer = best.is_none_or(|b: &MountEntry| {
                entry.mount_point.as_os_str().len() >= b.mount_point.as_os_str().len()
            });
            if longer {
                best = Some(entry);
            }
        }
        let entry = best?;
        let remainder = canonical
            .strip_prefix(&entry.mount_point)
            .expect("prefix matched above");
        Some((entry.dev, entry.fs_root.join(remainder)))
    }
}

/// Parses one mountinfo line (`id parent major:minor root
/// mountpoint ... - fstype source ...`); kernel-escapes
/// octal sequences in the path fields.
fn parse_mountinfo_line(line: &str) -> Option<MountEntry> {
    let before_dash = line.split(" - ").next()?;
    let mut fields = before_dash.split(' ');
    let _id = fields.next()?;
    let _parent = fields.next()?;
    let device = fields.next()?;
    let root = fields.next()?;
    let mount_point = fields.next()?;
    let (major, minor) = device.split_once(':')?;
    Some(MountEntry {
        mount_point: PathBuf::from(unescape_mountinfo_field(mount_point)),
        fs_root: PathBuf::from(unescape_mountinfo_field(root)),
        dev: (major.parse().ok()?, minor.parse().ok()?),
    })
}

/// Unescapes kernel mountinfo octal sequences (`\040` space,
/// `\011` tab, `\012` newline, `\134` backslash).
fn unescape_mountinfo_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(next) = chars.next() {
        if next != '\\' {
            out.push(next);
            continue;
        }
        let octal: String = chars.clone().take(3).collect();
        if octal.len() == 3
            && octal
                .bytes()
                .all(|byte| byte.is_ascii_digit() && byte < b'8')
            && let Ok(byte) = u8::from_str_radix(&octal, 8)
        {
            out.push(byte as char);
            chars.nth(2);
            continue;
        }
        out.push('\\');
    }
    out
}

/// Refuses read-write grafts aliasing non-FULL content
/// through bind-mount paths, pre-create (dentry-identity leg
/// of the graft-alias guard). Runs after the spelling guard:
/// every candidate already sits path-wise outside the
/// subtree. See the module docs for the threat shapes, the
/// sensitive-root accounting, and the skip conditions.
///
/// # Errors
///
/// Returns `CistellaError::Mount` naming the graft target on
/// the first aliased graft.
pub fn refuse_dentry_aliased_grafts(
    triples: &[MountTriple],
    ancestor_host: &Path,
    subtree_host: &Path,
) -> Result<()> {
    let candidates = triples.iter().any(|triple| {
        triple.mode == MountMode::Rw && dentry_id(Path::new(&triple.host_source)).is_some()
    });
    if !candidates {
        return Ok(());
    }
    let table = MountTable::read_host();
    refuse_dentry_aliases_with(
        triples,
        ancestor_host,
        subtree_host,
        table.as_ref(),
        &dentry_id,
    )
}

/// Testable core: dentry comparison with an injected mount
/// table and identity lookup. Production passes the live
/// table plus filesystem metadata; tests pass fixture tables
/// plus scripted ids (`None` injects a table read failure).
/// Decision order is load-bearing: contradictions with
/// read-only content refuse BEFORE any FULL-on-FULL
/// exemption admits — a second read-write bind cannot turn
/// an unsafe first one safe. Containment needs a usable
/// table (present, nonempty, resolving every compared side
/// with device agreement): otherwise the check refuses
/// typed rather than accepting unknown.
///
/// # Errors
///
/// Returns `CistellaError::Mount` naming the graft target on
/// the first aliased or unverifiable graft.
pub fn refuse_dentry_aliases_with(
    triples: &[MountTriple],
    ancestor_host: &Path,
    subtree_host: &Path,
    table: Option<&MountTable>,
    stat: &dyn Fn(&Path) -> Option<DentryId>,
) -> Result<()> {
    let ancestor_canon = canonicalize_host_source(&ancestor_host.to_string_lossy());
    let subtree_canon = canonicalize_host_source(&subtree_host.to_string_lossy());
    let ancestor_id = stat(&ancestor_canon);
    let subtree_id = stat(&subtree_canon);
    // An ancestor binding declared read-write carries its
    // own FULL carveout (compose grants every RW directory),
    // so its content is FULL-backed, not sensitive. Union
    // semantics: FULL wins over the read-execute route.
    let ancestor_full = triples.iter().any(|triple| {
        triple.mode == MountMode::Rw
            && canonicalize_host_source(&triple.host_source) == ancestor_canon
    });
    // FULL-backed graft sources by dentry accounting (their
    // carveouts), keyed by triple so a candidate never
    // matches itself. Read-only declarations: readability at
    // most — a second read-write bind of their content
    // contradicts the declaration, wherever the spelling
    // sits. Identity-only here; filesystem paths resolve
    // below, once, against the same table.
    let mut graft_ids: HashMap<usize, DentryId> = HashMap::new();
    let mut graft_sources: HashMap<usize, PathBuf> = HashMap::new();
    let mut ro_ids: Vec<DentryId> = Vec::new();
    let mut ro_sources: Vec<PathBuf> = Vec::new();
    for (index, triple) in triples.iter().enumerate() {
        let source = canonicalize_host_source(&triple.host_source);
        let Some(id) = stat(&source) else { continue };
        if triple.mode == MountMode::Rw {
            graft_ids.insert(index, id);
            graft_sources.insert(index, source);
        } else if triple.mode == MountMode::Ro
            && source != ancestor_canon
            && source != subtree_canon
        {
            // Same-spelling ancestor/subtree bindings are
            // the bindings themselves, not declarations
            // about their content: the ancestor's mode is
            // already accounted (`ancestor_full`), the
            // subtree is FULL by rule. Without this skip,
            // the ancestor's own read-only binding would
            // mark the whole tree sensitive and deny
            // explicitly admitted subtree grafts.
            ro_ids.push(id);
            ro_sources.push(source);
        }
    }
    for (index, triple) in triples.iter().enumerate() {
        if triple.mode != MountMode::Rw {
            continue;
        }
        let source = canonicalize_host_source(&triple.host_source);
        // Same spelling as the ancestor or subtree is the
        // binding itself, not an alias — its declared mode
        // governs (mirrors the spelling guard's exemption).
        // Only different-spelling dentries reach comparison.
        if source == ancestor_canon || source == subtree_canon {
            continue;
        }
        let Some(id) = stat(&source) else { continue };
        let under_subtree_path = source.starts_with(&subtree_canon);
        // Sensitive by exact identity (table-free): the
        // ancestor unless declared read-write itself, and
        // every read-only declaration. A paired read-write
        // alias of the same content does not dilute this —
        // exemptions below never precede it.
        let sensitive_eq =
            (!ancestor_full && ancestor_id.is_some_and(|root| root == id)) || ro_ids.contains(&id);
        if sensitive_eq {
            return Err(alias_refusal(&triple.container_target));
        }
        // Rule-backed FULL by exact identity (table-free):
        // the subtree, or the ancestor when declared
        // read-write. Decided without the table; everything
        // else needing proof falls through to containment.
        let rule_full_eq = subtree_id.is_some_and(|root| root == id)
            || (ancestor_full && ancestor_id.is_some_and(|root| root == id));
        // Containment needs a usable table: present,
        // nonempty, covering the candidate. Anything less
        // cannot prove disjointness — refuse typed rather
        // than accept unknown. (Graft-free topologies never
        // reach here; the wrapper returns early.)
        let Some(table) = table else {
            if rule_full_eq {
                continue;
            }
            return Err(table_refusal(&triple.container_target, "unavailable"));
        };
        if table.is_empty() {
            if rule_full_eq {
                continue;
            }
            return Err(table_refusal(&triple.container_target, "empty"));
        }
        let Some(candidate_fs) = table.filesystem_path(&source) else {
            return Err(table_refusal(
                &triple.container_target,
                "does not cover the graft",
            ));
        };
        if !devices_agree(id.dev, candidate_fs.0) {
            return Err(table_refusal(
                &triple.container_target,
                "disagrees with the filesystem",
            ));
        }
        // Resolve every compared side against the same
        // table; an unresolvable side cannot bound its
        // content — refuse rather than compare blind. Roots
        // with no dentry skip (nothing to alias, matching
        // the missing-path skip).
        let ancestor_fs = ancestor_id
            .and_then(|root| table.filesystem_path(&ancestor_canon).map(|fs| (root, fs)));
        let subtree_fs =
            subtree_id.and_then(|root| table.filesystem_path(&subtree_canon).map(|fs| (root, fs)));
        if ancestor_id.is_some() && ancestor_fs.is_none()
            || subtree_id.is_some() && subtree_fs.is_none()
        {
            return Err(table_refusal(
                &triple.container_target,
                "does not cover the roots",
            ));
        }
        let mut ro_fs = Vec::with_capacity(ro_sources.len());
        for (position, ro_source) in ro_sources.iter().enumerate() {
            let Some(resolved) = table.filesystem_path(ro_source) else {
                return Err(table_refusal(
                    &triple.container_target,
                    "does not cover a declaration",
                ));
            };
            if !devices_agree(ro_ids[position].dev, resolved.0) {
                return Err(table_refusal(
                    &triple.container_target,
                    "disagrees with the filesystem",
                ));
            }
            ro_fs.push(resolved);
        }
        let mut graft_fs: HashMap<usize, ((u64, u64), PathBuf)> = HashMap::new();
        for (other, graft_source) in &graft_sources {
            let Some(resolved) = table.filesystem_path(graft_source) else {
                return Err(table_refusal(
                    &triple.container_target,
                    "does not cover a declaration",
                ));
            };
            if !devices_agree(graft_ids[other].dev, resolved.0) {
                return Err(table_refusal(
                    &triple.container_target,
                    "disagrees with the filesystem",
                ));
            }
            graft_fs.insert(*other, resolved);
        }
        // Sensitive by containment: the ancestor tree under
        // a read-execute rule, or any read-only declaration.
        // Checked BEFORE the graft exemption — paired binds
        // of one sensitive dentry refuse on every spelling.
        // Proven subtree FULL subtracts from ancestor
        // sensitivity (the subtree rule overrides the
        // ancestor rule) but never from read-only
        // declarations (a second bind of RO content stays
        // contradictory wherever the spelling sits).
        let (candidate_dev, candidate_root) = &candidate_fs;
        let under_subtree = subtree_id.is_some_and(|root| root == id)
            || subtree_fs.as_ref().is_some_and(|(_, (dev, root))| {
                dev == candidate_dev && candidate_root.starts_with(root)
            });
        let sensitive = (!ancestor_full
            && ancestor_fs.as_ref().is_some_and(|(_, (dev, root))| {
                dev == candidate_dev && candidate_root.starts_with(root)
            })
            && !under_subtree)
            || ro_fs
                .iter()
                .any(|(dev, root)| dev == candidate_dev && candidate_root.starts_with(root));
        if sensitive {
            return Err(alias_refusal(&triple.container_target));
        }
        // FULL-backed by containment: the subtree, the
        // read-write-declared ancestor, or another
        // read-write graft's content (FULL-on-FULL, now
        // proven outside every sensitive root).
        let full = rule_full_eq
            || under_subtree
            || (ancestor_full
                && ancestor_fs.as_ref().is_some_and(|(_, (dev, root))| {
                    dev == candidate_dev && candidate_root.starts_with(root)
                }))
            || graft_fs.iter().any(|(other, (dev, root))| {
                *other != index && dev == candidate_dev && candidate_root.starts_with(root)
            });
        if under_subtree_path && !full {
            // Bind escape: path-wise inside the subtree but
            // the dentry lives outside FULL-backed content —
            // the carveout would grant undeclared content.
            return Err(CistellaError::Mount(format!(
                "read-write graft {} escapes the subtree through a bind mount: move the content inside the subtree or outside the tree",
                triple.container_target
            )));
        }
        // Outside the subtree with no sensitive hit: FULL-
        // backed and disjoint grafts alike are admissible —
        // declarations govern, Landlock enforces.
    }
    Ok(())
}

/// Refusal for a graft sharing dentries with read-only
/// content through a bind mount: value-free, names the
/// graft target and the remedy.
fn alias_refusal(target: &str) -> CistellaError {
    CistellaError::Mount(format!(
        "read-write graft {target} shares dentries with read-only content through a bind mount: move it outside the tree or inside the subtree"
    ))
}

/// Fail-closed refusal when dentry disjointness cannot be
/// proven: names the graft target and the table defect.
fn table_refusal(target: &str, defect: &str) -> CistellaError {
    CistellaError::Mount(format!(
        "cannot verify dentry disjointness for read-write graft {target}: host mount table {defect} — refusing pre-create"
    ))
}

/// Whether filesystem metadata agrees with the mount
/// table's device for one resolved path (guards stale or
/// mangled tables): compares the `st_dev` major/minor
/// against the entry's device numbers.
fn devices_agree(stat_dev: u64, table_dev: (u64, u64)) -> bool {
    use nix::libc::{dev_t, major, minor};
    // `dev_t` width varies by platform; the allow keeps
    // non-64-bit targets compiling without a lint break.
    #[allow(clippy::unnecessary_cast)]
    let raw = stat_dev as dev_t;
    #[allow(clippy::cast_possible_truncation)]
    let stat_major = major(raw) as u64;
    #[allow(clippy::cast_possible_truncation)]
    let stat_minor = minor(raw) as u64;
    let (major_number, minor_number) = table_dev;
    stat_major == major_number && stat_minor == minor_number
}
