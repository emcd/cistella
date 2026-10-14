//! Grant and retention behavior unit tests: FULL-source
//! accounting, revision/retention, Write normalization, and
//! same-target floor survival (split from mount at the
//! file-size limit). Topology validation stays in mount.

use cistella::mount::{MountMode, MountTriple};

#[test]
fn revise_retains_ro_under_full_routes() {
    use cistella::framework::landlock_compiler::revise_ro_for_confinement;
    use cistella::mount::{MountMode, MountTriple};
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("ro-data")).expect("ro-data");
    let host = dir.path().to_string_lossy().to_string();
    let triples = vec![
        MountTriple {
            host_source: host.clone(),
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: format!("{host}/ro-data"),
            container_target: "/src/proj/ro-data".to_string(),
            mode: MountMode::Ro,
        },
    ];
    // `/src/proj` receives FULL (subtree route): the nested RO
    // dir keeps Podman read-only (Landlock union cannot
    // subtract the parent grant, so the VFS binding carries
    // that enforcement); the ancestor outside FULL still flips
    // for submount materialization.
    let revised = revise_ro_for_confinement(&triples, &["/src/proj".to_string()], &[]);
    assert_eq!(revised[0].mode, MountMode::Rw);
    assert_eq!(revised[1].mode, MountMode::Ro);
}

#[test]
fn ro_confinement_preflight_refuses_rw_descendant() {
    use cistella::framework::landlock_compiler::ro_confinement_preflight;
    use cistella::mount::{MountMode, MountTriple};
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("ro-data")).expect("ro-data");
    let host = dir.path().to_string_lossy().to_string();
    let ro = MountTriple {
        host_source: format!("{host}/ro-data"),
        container_target: "/src/proj/ro-data".to_string(),
        mode: MountMode::Ro,
    };
    let rw = MountTriple {
        host_source: format!("{host}/ro-data/work"),
        container_target: "/src/proj/ro-data/work".to_string(),
        mode: MountMode::Rw,
    };
    let full = vec!["/src/proj".to_string()];
    // Contradictory topology (RW graft under retained-RO dir)
    // refuses with both targets named; clean topologies pass.
    let error = ro_confinement_preflight(&[ro.clone(), rw], &full, &[]).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("/src/proj/ro-data"), "got: {text}");
    assert!(text.contains("/src/proj/ro-data/work"), "got: {text}");
    ro_confinement_preflight(&[ro], &full, &[]).expect("retained RO without RW descendant passes");
}

#[test]
fn revise_retains_ro_for_alias_spellings_and_dev() {
    use cistella::framework::landlock_compiler::revise_ro_for_confinement;
    use cistella::mount::{MountMode, MountTriple};
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("ro-data")).expect("ro-data");
    std::fs::create_dir_all(dir.path().join("dev-data")).expect("dev-data");
    let host = dir.path().to_string_lossy().to_string();
    let triples = vec![
        // Traversal spelling of a target under FULL `/src/proj`:
        // canonical comparison retains Podman read-only.
        MountTriple {
            host_source: format!("{host}/ro-data"),
            container_target: "/src/../src/proj/ro-data".to_string(),
            mode: MountMode::Ro,
        },
        // Declared-RO directory under the fixed FULL `/dev`
        // baseline: retained (Landlock `/dev` FULL cannot
        // subtract, so the VFS binding carries enforcement).
        MountTriple {
            host_source: format!("{host}/dev-data"),
            container_target: "/dev/ro-data".to_string(),
            mode: MountMode::Ro,
        },
    ];
    let revised = revise_ro_for_confinement(
        &triples,
        &["/dev".to_string(), "/src/proj".to_string()],
        &[],
    );
    assert_eq!(revised[0].mode, MountMode::Ro);
    assert_eq!(revised[1].mode, MountMode::Ro);
}

#[test]
fn ro_confinement_preflight_collides_alias_spellings() {
    use cistella::framework::landlock_compiler::ro_confinement_preflight;
    use cistella::mount::{MountMode, MountTriple};
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("ro-data")).expect("ro-data");
    let host = dir.path().to_string_lossy().to_string();
    let ro = MountTriple {
        host_source: format!("{host}/ro-data"),
        container_target: "/src/proj/ro-data".to_string(),
        mode: MountMode::Ro,
    };
    // Same target through dot-segment spelling: canonical
    // comparison still collides with the retained-RO dir.
    let rw = MountTriple {
        host_source: format!("{host}/ro-data/work"),
        container_target: "/src//proj/./ro-data/work".to_string(),
        mode: MountMode::Rw,
    };
    let full = vec!["/src/proj".to_string()];
    ro_confinement_preflight(std::slice::from_ref(&ro), &full, &[]).expect("clean passes");
    let error = ro_confinement_preflight(&[ro, rw], &full, &[]).unwrap_err();
    assert!(
        error.to_string().contains("read-write descendant"),
        "got: {error}"
    );
}

#[test]
fn graft_alias_preflight_refuses_same_tree_rw_graft() {
    use cistella::mount::{MountMode, MountTriple, graft_alias_preflight};
    use std::path::Path;
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("proj")).expect("proj");
    std::fs::create_dir_all(tree.path().join("proj/extra")).expect("extra");
    std::fs::create_dir_all(tree.path().join("sib")).expect("sib");
    let outside = tempfile::tempdir().expect("outside");
    let host = tree.path().to_string_lossy().to_string();
    let ancestor = Path::new(&host);
    let subtree = &tree.path().join("proj");
    let aliasing = vec![
        MountTriple {
            host_source: host.clone(),
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: format!("{host}/sib"),
            container_target: "/src/graft".to_string(),
            mode: MountMode::Rw,
        },
    ];
    // Same-tree RW graft outside the subtree: the graft FULL
    // would dentry-alias RO-covered content — refuse naming
    // the graft target.
    let error = graft_alias_preflight(&aliasing, ancestor, subtree).unwrap_err();
    assert!(error.to_string().contains("/src/graft"), "got: {error}");
    // Disjoint graft (outside the tree): disjoint dentries, no
    // alias — passes.
    let disjoint = vec![
        aliasing[0].clone(),
        MountTriple {
            host_source: outside.path().to_string_lossy().to_string(),
            container_target: "/src/graft".to_string(),
            mode: MountMode::Rw,
        },
    ];
    graft_alias_preflight(&disjoint, ancestor, subtree).expect("disjoint graft passes");
    // Subtree content RW (already FULL): no new writability —
    // passes. Ancestor RW itself (own FULL route): passes. RO
    // same-tree graft (union-safe): passes. Not-yet-existing
    // source (no dentry to alias): skips.
    let benign = vec![
        MountTriple {
            host_source: host.clone(),
            container_target: "/src".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/proj/extra"),
            container_target: "/src/proj/extra".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/sib"),
            container_target: "/src/ro-graft".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: format!("{host}/missing"),
            container_target: "/src/missing".to_string(),
            mode: MountMode::Rw,
        },
    ];
    graft_alias_preflight(&benign, ancestor, subtree).expect("benign shapes pass");
}

#[test]
fn revise_retains_ro_under_full_backed_source() {
    use cistella::framework::contract::EffectivePermissions;
    use cistella::framework::landlock_compiler::{full_grant_sources, revise_ro_for_confinement};
    use cistella::mount::{MountMode, MountTriple, Placement};
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("proj")).expect("proj");
    std::fs::create_dir_all(tree.path().join("proj/private")).expect("private");
    let host = tree.path().to_string_lossy().to_string();
    let subtree = tree.path().join("proj");
    // RO directory inside the FULL subtree bound outside all
    // FULL routes: its dentries are FULL through the subtree
    // rule regardless of target — retain Podman read-only (the
    // reverse-direction alias of the graft case).
    let triples = vec![MountTriple {
        host_source: format!("{host}/proj/private"),
        container_target: "/extra/private".to_string(),
        mode: MountMode::Ro,
    }];
    let placements: Vec<Placement> = triples
        .iter()
        .map(|triple| Placement {
            host_source: triple.host_source.clone(),
            container_target: triple.container_target.clone(),
        })
        .collect();
    let effective =
        EffectivePermissions::assemble(&triples, &[], &[], &[]).expect("driver-only assembly");
    let sources = full_grant_sources(&placements, &effective, &subtree);
    assert_eq!(sources, vec![subtree.clone()]);
    let revised = revise_ro_for_confinement(&triples, &["/src/proj".to_string()], &sources);
    assert_eq!(revised[0].mode, MountMode::Ro);
}

#[test]
fn full_grant_sources_cover_subtree_and_rw_dirs() {
    use cistella::framework::contract::EffectivePermissions;
    use cistella::framework::landlock_compiler::full_grant_sources;
    use cistella::mount::{MountMode, MountTriple, Placement};
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("proj")).expect("proj");
    std::fs::create_dir_all(tree.path().join("state")).expect("state");
    std::fs::write(tree.path().join("sock"), b"x").expect("sock");
    let host = tree.path().to_string_lossy().to_string();
    let subtree = tree.path().join("proj");
    let triples = vec![
        MountTriple {
            host_source: format!("{host}/state"),
            container_target: "/src/state".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/other"),
            container_target: "/other".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: tree.path().join("sock").to_string_lossy().to_string(),
            container_target: "/run/agent.sock".to_string(),
            mode: MountMode::Rw,
        },
    ];
    let placements: Vec<Placement> = triples
        .iter()
        .map(|triple| Placement {
            host_source: triple.host_source.clone(),
            container_target: triple.container_target.clone(),
        })
        .collect();
    let effective =
        EffectivePermissions::assemble(&triples, &[], &[], &[]).expect("driver-only assembly");
    let sources = full_grant_sources(&placements, &effective, &subtree);
    assert!(sources.contains(&subtree), "{sources:?}");
    assert!(sources.contains(&tree.path().join("state")), "{sources:?}");
    assert_eq!(
        sources.len(),
        2,
        "RO triples and files add nothing: {sources:?}"
    );
}

#[test]
fn ro_confinement_preflight_catches_source_retained_contradiction() {
    use cistella::framework::landlock_compiler::ro_confinement_preflight;
    use cistella::mount::{MountMode, MountTriple};
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("proj")).expect("proj");
    std::fs::create_dir_all(tree.path().join("proj/private")).expect("private");
    let host = tree.path().to_string_lossy().to_string();
    let subtree = tree.path().join("proj");
    let ro = MountTriple {
        host_source: format!("{host}/proj/private"),
        container_target: "/extra/private".to_string(),
        mode: MountMode::Ro,
    };
    let rw = MountTriple {
        host_source: format!("{host}/proj/private/work"),
        container_target: "/extra/private/work".to_string(),
        mode: MountMode::Rw,
    };
    // Source-retained (under subtree) with an RW descendant
    // beneath its target: same contradiction as target
    // retention — refuse.
    let sources = vec![subtree];
    let error =
        ro_confinement_preflight(&[ro, rw], &["/src/proj".to_string()], &sources).unwrap_err();
    assert!(error.to_string().contains("/extra/private"), "got: {error}");
}

#[test]
fn identical_ro_sources_admit_while_write_side_refuses() {
    use cistella::mount::validate_mounts;
    // Identical sources bound twice read-only: union is
    // read-only, no rights ambiguity — admitted (the alias
    // retention proof needs one source at two RO targets).
    // Any write side keeps the ambiguous-intent refusal.
    let tree = tempfile::tempdir().expect("tree");
    let host = tree.path().to_string_lossy().to_string();
    std::fs::create_dir(tree.path().join("shared")).expect("shared");
    let ro_a = MountTriple {
        host_source: format!("{host}/shared"),
        container_target: "/tmp".to_string(),
        mode: MountMode::Ro,
    };
    let ro_b = MountTriple {
        host_source: format!("{host}/shared"),
        container_target: "/mirror".to_string(),
        mode: MountMode::Ro,
    };
    assert!(
        validate_mounts(&[ro_a.clone(), ro_b.clone()], "/home/cistella").is_ok(),
        "identical RO+RO admits"
    );
    let rw_b = MountTriple {
        host_source: format!("{host}/shared"),
        container_target: "/mirror".to_string(),
        mode: MountMode::Rw,
    };
    assert!(
        validate_mounts(&[ro_a, rw_b], "/home/cistella").is_err(),
        "RO+RW identical sources still refuse"
    );
}

#[test]
fn write_normalizes_to_full_before_all_consumers() {
    // A bare `Write` placement joins FULL accounting exactly
    // like `Full` (normalized before every consumer): its host
    // source is FULL-backed despite the non-FULL label, so no
    // alias hole opens.
    use cistella::framework::contract::{
        EffectivePermissions, PermissionContribution, Provenance, Rights,
    };
    use cistella::framework::landlock_compiler::full_grant_sources;
    use cistella::mount::Placement;
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("state")).expect("state");
    let host = tree.path().to_string_lossy().to_string();
    let placements = vec![Placement {
        host_source: format!("{host}/state"),
        container_target: "/state".to_string(),
    }];
    let permissions = vec![PermissionContribution {
        target: "/state".to_string(),
        rights: Rights::Write,
        provenance: Provenance::Extension("probe".to_string()),
    }];
    let effective =
        EffectivePermissions::assemble(&[], &placements, &permissions, &[]).expect("assembles");
    let grant = effective.get("/state").expect("grant exists");
    assert_eq!(grant.rights, Rights::Full, "Write normalizes to Full");
    assert!(
        grant
            .claimants
            .iter()
            .any(
                |(provenance, rights)| *provenance == Provenance::Extension("probe".to_string())
                    && *rights == Rights::Write
            ),
        "original Write label survives for audit: {:?}",
        grant.claimants
    );
    let sources = full_grant_sources(&placements, &effective, tree.path());
    assert!(
        sources.contains(&tree.path().join("state")),
        "Write source joins FULL accounting: {sources:?}"
    );
}

#[test]
fn same_target_driver_ro_plus_extension_write_stays_vfs_ro() {
    // Driver RO plus extension Write on the same target under
    // FULL: Landlock grants Full (joined minimum) while the
    // Podman binding stays read-only — the join widens the
    // grant without erasing the wall.
    use cistella::framework::contract::{
        EffectivePermissions, PermissionContribution, Provenance, Rights,
    };
    use cistella::framework::landlock_compiler::{full_grant_sources, revise_ro_for_confinement};
    use cistella::mount::{MountMode, MountTriple, Placement};
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("proj")).expect("proj");
    std::fs::create_dir_all(tree.path().join("mirror")).expect("mirror");
    let host = tree.path().to_string_lossy().to_string();
    let subtree = tree.path().join("proj");
    let triples = vec![MountTriple {
        host_source: format!("{host}/mirror"),
        container_target: "/mirror".to_string(),
        mode: MountMode::Ro,
    }];
    let permissions = vec![PermissionContribution {
        target: "/mirror".to_string(),
        rights: Rights::Write,
        provenance: Provenance::Extension("probe".to_string()),
    }];
    let placements: Vec<Placement> = triples
        .iter()
        .map(|triple| Placement {
            host_source: triple.host_source.clone(),
            container_target: triple.container_target.clone(),
        })
        .collect();
    let effective = EffectivePermissions::assemble(&triples, &placements, &permissions, &[])
        .expect("assembles");
    let grant = effective.get("/mirror").expect("grant exists");
    assert_eq!(grant.rights, Rights::Full, "join yields Full");
    assert!(
        grant.binding_intent_ro,
        "driver RO intent survives the join"
    );
    let sources = full_grant_sources(&placements, &effective, &subtree);
    let revised = revise_ro_for_confinement(&triples, &["/mirror".to_string()], &sources);
    assert_eq!(
        revised[0].mode,
        MountMode::Ro,
        "retention keeps the RO binding under FULL"
    );
}
