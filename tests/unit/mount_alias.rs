//! Dentry-identity hardening pins: mountinfo parsing plus the
//! bind-alias refusal shapes (fixture tables and scripted
//! dentry ids — real bind mounts need privilege, so the live
//! adversarial-bind proof rides the 3.4 denial matrix).
//!
//! Scripted `st_dev` values are real `makedev` encodings
//! (`(major << 8) | minor` for these small numbers) because
//! the implementation cross-checks metadata devices against
//! the table's `major:minor` pairs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cistella::mount::{MountMode, MountTriple, canonicalize_host_source};
use cistella::mount_alias::{DentryId, MountTable, refuse_dentry_aliases_with};

/// Host layout for the pins: `/tree` ancestor (dev 8:1),
/// `/tree/proj` subtree, `/tree/sib` denied sibling (ino
/// 102), `/opt` outside content (dev 8:2), plus a bind of
/// the sibling at `/mnt/alias` and a bind escape of
/// `/opt/evil` at `/tree/proj/evil`.
const FIXTURE_MOUNTINFO: &str = "31 23 8:1 / / rw,relatime - ext4 /dev/sda1 rw
32 23 8:1 /tree/sib /mnt/alias rw,relatime - ext4 /dev/sda1 rw
33 23 8:2 / /opt rw,relatime - ext4 /dev/sda2 rw
34 23 8:2 /evil /tree/proj/evil rw,relatime - ext4 /dev/sda2 rw
";

/// Paired binds of one sibling at two outside paths (the
/// HIGH-1 shape: each spelling must refuse on its own —
/// neither may exempt the other).
const PAIRED_MOUNTINFO: &str = "31 23 8:1 / / rw,relatime - ext4 /dev/sda1 rw
32 23 8:1 /tree/sib /mnt/a rw,relatime - ext4 /dev/sda1 rw
33 23 8:1 /tree/sib /mnt/b rw,relatime - ext4 /dev/sda1 rw
";

/// Paired binds of one read-only declaration at two outside
/// paths (the HIGH-1 RO shape).
const PAIRED_RO_MOUNTINFO: &str = "31 23 8:1 / / rw,relatime - ext4 /dev/sda1 rw
32 23 8:2 / /opt rw,relatime - ext4 /dev/sda2 rw
33 23 8:2 /ro /srv/x rw,relatime - ext4 /dev/sda2 rw
34 23 8:2 /ro /srv/y rw,relatime - ext4 /dev/sda2 rw
";

/// Second bind of legitimate outside graft content (the
/// FULL-on-FULL shape that must keep passing).
const MIRROR_MOUNTINFO: &str = "31 23 8:1 / / rw,relatime - ext4 /dev/sda1 rw
32 23 8:2 / /opt rw,relatime - ext4 /dev/sda2 rw
33 23 8:2 /state /srv/mirror rw,relatime - ext4 /dev/sda2 rw
";

/// `makedev` encoding for the small test devices.
fn dev(major: u64, minor: u64) -> u64 {
    (major << 8) | minor
}

fn id(dev: u64, ino: u64) -> DentryId {
    DentryId { dev, ino }
}

fn stat_of(pairs: &[(&str, DentryId)]) -> HashMap<PathBuf, DentryId> {
    pairs
        .iter()
        .map(|(path, ident)| (canonicalize_host_source(path), *ident))
        .collect()
}

fn check(
    triples: &[MountTriple],
    table: Option<&MountTable>,
    stats: &HashMap<PathBuf, DentryId>,
) -> Result<(), cistella::error::CistellaError> {
    refuse_dentry_aliases_with(
        triples,
        Path::new("/tree"),
        Path::new("/tree/proj"),
        table,
        &|path| stats.get(path).copied(),
    )
}

fn rw(source: &str, target: &str) -> MountTriple {
    MountTriple {
        host_source: source.to_string(),
        container_target: target.to_string(),
        mode: MountMode::Rw,
    }
}

fn ro(source: &str, target: &str) -> MountTriple {
    MountTriple {
        host_source: source.to_string(),
        container_target: target.to_string(),
        mode: MountMode::Ro,
    }
}

#[test]
fn parse_mountinfo_resolves_bind_sources() {
    let table = MountTable::parse(FIXTURE_MOUNTINFO).expect("fixture parses");
    // Bind alias resolves to the source's filesystem-relative
    // path on the source device …
    let (dev, fs) = table
        .filesystem_path(Path::new("/mnt/alias"))
        .expect("alias resolves");
    assert_eq!(dev, (8, 1));
    assert_eq!(fs, PathBuf::from("/tree/sib"));
    // … while the ancestor resolves through the root mount on
    // the same device, so containment proves the alias.
    let (anc_dev, anc_fs) = table
        .filesystem_path(Path::new("/tree"))
        .expect("ancestor resolves");
    assert_eq!((anc_dev, anc_fs.clone()), ((8, 1), PathBuf::from("/tree")));
    assert!(fs.starts_with(&anc_fs));
    // Outside content on another device resolves disjointly.
    let (opt_dev, opt_fs) = table
        .filesystem_path(Path::new("/opt/state"))
        .expect("outside resolves");
    assert_eq!(opt_dev, (8, 2));
    assert!(!opt_fs.starts_with(&anc_fs));
}

#[test]
fn parse_mountinfo_unescapes_paths() {
    let table =
        MountTable::parse("40 23 8:1 /my\\040dir /mnt/space\\040dir rw - ext4 /dev/sda1 rw\n")
            .expect("escape fixture parses");
    let (_, fs) = table
        .filesystem_path(Path::new("/mnt/space dir"))
        .expect("escaped mountpoint resolves");
    assert_eq!(fs, PathBuf::from("/my dir"));
}

#[test]
fn refuses_bind_aliased_graft_outside_subtree() {
    let table = MountTable::parse(FIXTURE_MOUNTINFO).expect("fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/mnt/alias", id(dev(8, 1), 102)),
    ]);
    // /mnt/alias is a bind of /tree/sib: spelling sits
    // outside the ancestor while the dentry sits under it —
    // the graft FULL would admit the denied sibling.
    let triples = vec![ro("/tree", "/src"), rw("/mnt/alias", "/src/graft")];
    let error = check(&triples, Some(&table), &stats).unwrap_err();
    assert!(error.to_string().contains("/src/graft"), "got: {error}");
}

#[test]
fn refuses_exact_dir_bind_without_table() {
    // Same dentry as the ancestor under another path: the
    // dev+ino leg needs no mount table.
    let stats = stat_of(&[
        ("/tree", id(7, 50)),
        ("/tree/proj", id(7, 51)),
        ("/srv/treebind", id(7, 50)),
    ]);
    let triples = vec![ro("/tree", "/src"), rw("/srv/treebind", "/src/graft")];
    let error = check(&triples, None, &stats).unwrap_err();
    assert!(error.to_string().contains("/src/graft"), "got: {error}");
}

#[test]
fn refuses_paired_rw_binds_of_sibling() {
    // Two read-write binds of one denied sibling: each
    // spelling refuses on its own — the pair must not
    // exempt each other as FULL-on-FULL.
    let table = MountTable::parse(PAIRED_MOUNTINFO).expect("paired fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/mnt/a", id(dev(8, 1), 102)),
        ("/mnt/b", id(dev(8, 1), 102)),
    ]);
    let triples = vec![
        ro("/tree", "/src"),
        rw("/mnt/a", "/src/a"),
        rw("/mnt/b", "/src/b"),
    ];
    let error = check(&triples, Some(&table), &stats).unwrap_err();
    assert!(error.to_string().contains("/src/a"), "got: {error}");
    // Either spelling alone refuses too.
    let single = vec![ro("/tree", "/src"), rw("/mnt/b", "/src/b")];
    let error = check(&single, Some(&table), &stats).unwrap_err();
    assert!(error.to_string().contains("/src/b"), "got: {error}");
}

#[test]
fn refuses_paired_rw_binds_of_ro_declaration() {
    // Two read-write binds of one read-only declaration:
    // the RO contradiction refuses before any FULL-on-FULL
    // exemption admits.
    let table = MountTable::parse(PAIRED_RO_MOUNTINFO).expect("paired RO fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/opt/ro", id(dev(8, 2), 400)),
        ("/srv/x", id(dev(8, 2), 400)),
        ("/srv/y", id(dev(8, 2), 400)),
    ]);
    let triples = vec![
        ro("/tree", "/src"),
        ro("/opt/ro", "/extra/ro"),
        rw("/srv/x", "/src/x"),
        rw("/srv/y", "/src/y"),
    ];
    let error = check(&triples, Some(&table), &stats).unwrap_err();
    assert!(error.to_string().contains("/src/x"), "got: {error}");
}

#[test]
fn refuses_subtree_escape_bind() {
    let table = MountTable::parse(FIXTURE_MOUNTINFO).expect("fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/tree/proj/evil", id(dev(8, 2), 300)),
    ]);
    // Path-wise inside the subtree, dentry outside it: the
    // carveout would grant undeclared content FULL.
    let triples = vec![ro("/tree", "/src"), rw("/tree/proj/evil", "/src/proj/evil")];
    let error = check(&triples, Some(&table), &stats).unwrap_err();
    assert!(error.to_string().contains("/src/proj/evil"), "got: {error}");
}

#[test]
fn refuses_bind_of_ro_declared_content() {
    // A second read-write bind of read-only-declared
    // content contradicts the declaration, wherever the
    // spelling sits — dev+ino equality suffices.
    let stats = stat_of(&[
        ("/tree", id(7, 50)),
        ("/tree/proj", id(7, 51)),
        ("/opt/ro", id(9, 400)),
        ("/srv/copy", id(9, 400)),
    ]);
    let triples = vec![
        ro("/tree", "/src"),
        ro("/opt/ro", "/extra/ro"),
        rw("/srv/copy", "/src/graft"),
    ];
    let error = check(&triples, None, &stats).unwrap_err();
    assert!(error.to_string().contains("/src/graft"), "got: {error}");
}

#[test]
fn refuses_alias_when_table_unreadable() {
    // Different inode from every sensitive root: exact
    // identity cannot decide, and no table can prove
    // disjointness — fail typed, not open.
    let stats = stat_of(&[
        ("/tree", id(7, 50)),
        ("/tree/proj", id(7, 51)),
        ("/mnt/alias", id(7, 102)),
    ]);
    let triples = vec![ro("/tree", "/src"), rw("/mnt/alias", "/src/graft")];
    let error = check(&triples, None, &stats).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("/src/graft"), "got: {text}");
    assert!(text.contains("mount table unavailable"), "got: {text}");
}

#[test]
fn refuses_alias_when_table_empty() {
    let table = MountTable::parse("").expect("empty parses");
    assert!(table.is_empty());
    let stats = stat_of(&[
        ("/tree", id(7, 50)),
        ("/tree/proj", id(7, 51)),
        ("/mnt/alias", id(7, 102)),
    ]);
    let triples = vec![ro("/tree", "/src"), rw("/mnt/alias", "/src/graft")];
    let error = check(&triples, Some(&table), &stats).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("/src/graft"), "got: {text}");
    assert!(text.contains("mount table empty"), "got: {text}");
}

#[test]
fn refuses_alias_when_table_does_not_cover() {
    // Table missing every entry covering the graft: the
    // alias is unresolvable — unprovable disjointness.
    let table = MountTable::parse("33 23 8:2 / /opt rw,relatime - ext4 /dev/sda2 rw\n")
        .expect("partial fixture parses");
    let stats = stat_of(&[
        ("/tree", id(7, 50)),
        ("/tree/proj", id(7, 51)),
        ("/mnt/alias", id(7, 102)),
    ]);
    let triples = vec![ro("/tree", "/src"), rw("/mnt/alias", "/src/graft")];
    let error = check(&triples, Some(&table), &stats).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("/src/graft"), "got: {text}");
    assert!(text.contains("does not cover"), "got: {text}");
}

#[test]
fn refuses_alias_when_table_disagrees() {
    // Table resolves the graft to a device the filesystem
    // metadata contradicts: stale or mangled table — refuse.
    let table = MountTable::parse(FIXTURE_MOUNTINFO).expect("fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/mnt/alias", id(dev(9, 9), 102)),
    ]);
    let triples = vec![ro("/tree", "/src"), rw("/mnt/alias", "/src/graft")];
    let error = check(&triples, Some(&table), &stats).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("/src/graft"), "got: {text}");
    assert!(
        text.contains("disagrees with the filesystem"),
        "got: {text}"
    );
}

#[test]
fn admits_disjoint_same_filesystem_graft() {
    let table = MountTable::parse(FIXTURE_MOUNTINFO).expect("fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/srv/state", id(dev(8, 1), 500)),
        ("/opt/state", id(dev(8, 2), 501)),
    ]);
    // Same filesystem as the ancestor but disjoint
    // filesystem-relative paths (the Dropbox-Notes shape),
    // plus outside content on another device: no shared
    // dentries, no alias — both pass.
    let triples = vec![
        ro("/tree", "/src"),
        rw("/srv/state", "/src/state"),
        rw("/opt/state", "/src/other"),
    ];
    check(&triples, Some(&table), &stats).expect("disjoint grafts pass");
}

#[test]
fn refuses_malformed_table_with_valid_root() {
    // A truncated bind-mount line plus a valid same-device
    // root: strict parse rejects the whole table rather
    // than falling back to the root prefix and mapping the
    // alias to a fake filesystem-relative path.
    let text = "31 23 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n32 23 8:1 /tree/sib\n";
    assert!(MountTable::parse(text).is_none(), "malformed rejects");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/mnt/alias", id(dev(8, 1), 102)),
    ]);
    let triples = vec![ro("/tree", "/src"), rw("/mnt/alias", "/src/graft")];
    let error = check(&triples, None, &stats).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("/src/graft"), "got: {text}");
    assert!(text.contains("mount table unavailable"), "got: {text}");
}

#[test]
fn admits_subtree_descendant_graft() {
    // Ordinary layout: read-only ancestor, read-write
    // subtree, read-write graft of a subtree descendant
    // (different inode, same filesystem). Proven subtree
    // FULL overrides the ancestor rule — the graft passes.
    let table = MountTable::parse(FIXTURE_MOUNTINFO).expect("fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/tree/proj/extra", id(dev(8, 1), 103)),
    ]);
    let triples = vec![
        ro("/tree", "/src"),
        rw("/tree/proj", "/src/proj"),
        rw("/tree/proj/extra", "/src/proj/extra"),
    ];
    check(&triples, Some(&table), &stats).expect("subtree descendant passes");
}

#[test]
fn admits_subtree_content_through_bind() {
    // A second bind of subtree content outside the subtree
    // is FULL by the subtree rule itself — decided on
    // identity alone, no table needed.
    let stats = stat_of(&[
        ("/tree", id(7, 50)),
        ("/tree/proj", id(7, 51)),
        ("/mnt/projbind", id(7, 51)),
    ]);
    let triples = vec![ro("/tree", "/src"), rw("/mnt/projbind", "/src/graft")];
    check(&triples, None, &stats).expect("subtree bind passes");
}

#[test]
fn admits_second_rw_bind_of_graft_content() {
    // Two read-write grafts of the same outside content,
    // proven outside every sensitive root: FULL-on-FULL is
    // declared intent twice — passes.
    let table = MountTable::parse(MIRROR_MOUNTINFO).expect("mirror fixture parses");
    let stats = stat_of(&[
        ("/tree", id(dev(8, 1), 100)),
        ("/tree/proj", id(dev(8, 1), 101)),
        ("/opt/state", id(dev(8, 2), 200)),
        ("/srv/mirror", id(dev(8, 2), 200)),
    ]);
    let triples = vec![
        ro("/tree", "/src"),
        rw("/opt/state", "/src/state"),
        rw("/srv/mirror", "/src/mirror"),
    ];
    check(&triples, Some(&table), &stats).expect("RW-on-RW alias passes");
}

#[test]
fn admits_rw_ancestor_binding_itself() {
    // The ancestor declared read-write carries its own FULL
    // carveout: a same-dentry graft is that intent restated,
    // not an alias of read-only content.
    let stats = stat_of(&[("/tree", id(7, 50)), ("/tree/proj", id(7, 51))]);
    let triples = vec![rw("/tree", "/src"), rw("/tree", "/mirror")];
    check(&triples, None, &stats).expect("RW ancestor restatement passes");
}

#[test]
fn skips_graft_free_topologies_without_table() {
    // No read-write directory grafts: nothing to compare —
    // passes without touching the mount table (keeps the
    // fast suite portable to seats without /proc).
    let stats = stat_of(&[("/tree", id(7, 50)), ("/tree/proj", id(7, 51))]);
    let triples = vec![ro("/tree", "/src")];
    check(&triples, None, &stats).expect("graft-free passes");
}

#[test]
#[cfg(target_os = "linux")]
fn live_wrapper_passes_disjoint_graft() {
    // End to end through real filesystem metadata (Linux:
    // the mount table exists, so disjointness is provable):
    // a disjoint outside graft passes.
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("proj")).expect("proj");
    let outside = tempfile::tempdir().expect("outside");
    let triples = vec![
        ro(&tree.path().to_string_lossy(), "/src"),
        rw(&outside.path().to_string_lossy(), "/src/graft"),
    ];
    cistella::mount_alias::refuse_dentry_aliased_grafts(
        &triples,
        tree.path(),
        &tree.path().join("proj"),
    )
    .expect("disjoint live graft passes");
}
