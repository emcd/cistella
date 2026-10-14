//! Compile snapshot/binding unit tests: frozen snapshots,
//! fingerprint dimensions, output coverage and consumer
//! binding, single-claimant compile dispatch (split from
//! prepare_permissions at the file-size limit). Grant joining
//! stays in prepare_permissions.

fn singleton_hook(prefix: Vec<String>) -> cistella::framework::contract::GuestHookRequest {
    use cistella::framework::contract::{HookArtifact, HookProbe, HookSource};
    cistella::framework::contract::GuestHookRequest {
        artifact: HookArtifact {
            kind: "digest-pinned-blob".to_string(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            source: HookSource {
                registry: "shipped".to_string(),
                path: "cistella-supervisor-landlock".to_string(),
            },
        },
        staging: "isolator-staged".to_string(),
        order: 0,
        argv_prefix: prefix,
        probe: HookProbe {
            op: "probe_capabilities".to_string(),
            timeout_ms: 10_000,
        },
        on_failure: "fail-pre-exec".to_string(),
    }
}

fn test_hook() -> cistella::framework::contract::GuestHookRequest {
    singleton_hook(vec!["/run/cistella/hooks/supervisor-landlock".to_string()])
}

fn compile_triples() -> Vec<cistella::mount::MountTriple> {
    vec![cistella::mount::MountTriple {
        host_source: "/home/op/src".to_string(),
        container_target: "/src".to_string(),
        mode: cistella::mount::MountMode::Ro,
    }]
}

fn compile_inputs() -> (
    Vec<cistella::mount::Placement>,
    cistella::framework::contract::EffectivePermissions,
) {
    use cistella::framework::contract::{
        EffectivePermissions, PermissionContribution, Provenance, Rights,
    };
    use cistella::mount::Placement;
    let placements = vec![Placement {
        host_source: "/home/op/src".to_string(),
        container_target: "/src".to_string(),
    }];
    let permissions = vec![PermissionContribution {
        target: "/src".to_string(),
        rights: Rights::Read,
        provenance: Provenance::Extension("probe".to_string()),
    }];
    let effective =
        EffectivePermissions::assemble(&[], &placements, &permissions, &[]).expect("assembles");
    (placements, effective)
}

#[test]
fn freeze_snapshot_refuses_duplicate_ids() {
    use cistella::framework::landlock_compiler::freeze_snapshot;
    let (mut placements, effective) = compile_inputs();
    placements.push(placements[0].clone());
    let error = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &test_hook(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("duplicate placement target"),
        "got: {error}"
    );
}

#[test]
fn freeze_snapshot_refuses_grant_gap() {
    use cistella::framework::landlock_compiler::freeze_snapshot;
    use cistella::mount::Placement;
    let (placements, _) = compile_inputs();
    let mut orphan = placements;
    orphan.push(Placement {
        host_source: "/srv/other".to_string(),
        container_target: "/other".to_string(),
    });
    let (_, effective) = compile_inputs();
    let error = freeze_snapshot(
        &orphan,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &test_hook(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("no effective grant"),
        "got: {error}"
    );
}

#[test]
fn joined_parent_with_rw_child_refuses_despite_full_grants() {
    // Both axes resolved: the joined-Full parent renders
    // read-only from surviving intent, so the binding-level
    // contradiction (RO binding, RW descendant) refuses even
    // though grants are Full on both. Grant aliasing stays the
    // graft preflight's job; this gate reads bindings.
    use cistella::framework::contract::{
        EffectivePermissions, MergeContext, PermissionContribution, Provenance, Rights,
        merge_prepare, render_placements,
    };
    use cistella::framework::landlock_compiler::ro_confinement_preflight;
    use cistella::mount::Placement;
    use std::collections::HashSet;
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("state/sub")).expect("state");
    let host = tree.path().to_string_lossy().to_string();
    let plan = cistella::framework::contract::PreparePlan {
        mounts: vec![
            cistella::framework::contract::MountContribution {
                placement: Placement {
                    host_source: format!("{host}/state"),
                    container_target: "/state".to_string(),
                },
                provenance: Provenance::Extension("guest-a".to_string()),
            },
            cistella::framework::contract::MountContribution {
                placement: Placement {
                    host_source: format!("{host}/state/sub"),
                    container_target: "/state/sub".to_string(),
                },
                provenance: Provenance::Extension("guest-b".to_string()),
            },
        ],
        permissions: vec![
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Read,
                provenance: Provenance::Extension("guest-a".to_string()),
            },
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Write,
                provenance: Provenance::Extension("guest-b".to_string()),
            },
            PermissionContribution {
                target: "/state/sub".to_string(),
                rights: Rights::Full,
                provenance: Provenance::Extension("guest-b".to_string()),
            },
        ],
        ..cistella::framework::contract::PreparePlan::default()
    };
    let context = MergeContext::new("/home/cistella", HashSet::new(), Vec::new());
    let merged = merge_prepare(
        plan,
        &cistella::framework::contract::CapabilitySet::new(&[
            cistella::framework::contract::Capability::Mounts,
            cistella::framework::contract::Capability::Permissions,
        ]),
        &context,
    )
    .expect("join admits");
    let effective = EffectivePermissions::assemble(
        &[],
        &merged.placements,
        &[
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Read,
                provenance: Provenance::Extension("guest-a".to_string()),
            },
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Write,
                provenance: Provenance::Extension("guest-b".to_string()),
            },
            PermissionContribution {
                target: "/state/sub".to_string(),
                rights: Rights::Full,
                provenance: Provenance::Extension("guest-b".to_string()),
            },
        ],
        &[],
    )
    .expect("reassembles");
    let rendered = render_placements(&merged.placements, &effective);
    assert_eq!(rendered.len(), 2);
    let error = ro_confinement_preflight(
        &rendered,
        &["/state".to_string()],
        &[tree.path().join("state")],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("/state/sub"),
        "RW descendant under retained-RO parent refuses, got: {error}"
    );
}

#[test]
fn compiled_binding_refuses_stale_fingerprint() {
    use cistella::framework::hooks::verify_compiled_binding;
    use cistella::framework::landlock_compiler::{compile_topology, freeze_snapshot};
    let (placements, effective) = compile_inputs();
    let hook = test_hook();
    let snapshot = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .expect("freezes");
    let mut compiled = compile_topology(&snapshot, &hook).expect("compiles");
    compiled.fingerprint = "stale".to_string();
    let error = verify_compiled_binding(&snapshot, &compiled, &hook).unwrap_err();
    assert!(
        error.to_string().contains("escapes its snapshot binding"),
        "got: {error}"
    );
}

#[test]
fn compiled_binding_refuses_foreign_supervisor() {
    // CompilerA rendering paired with a SupervisorB policy: the
    // hook digest binds output to its enforcement consumer.
    use cistella::framework::hooks::verify_compiled_binding;
    use cistella::framework::landlock_compiler::{compile_topology, freeze_snapshot};
    let (placements, effective) = compile_inputs();
    let hook = test_hook();
    let snapshot = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .expect("freezes");
    let compiled = compile_topology(&snapshot, &hook).expect("compiles");
    let mut other = test_hook();
    other.artifact.sha256 = "f".repeat(64);
    let error = verify_compiled_binding(&snapshot, &compiled, &other).unwrap_err();
    assert!(
        error.to_string().contains("escapes its snapshot binding"),
        "got: {error}"
    );
}

#[test]
fn compile_phase_supports_exactly_one_hook() {
    use cistella::framework::hooks::prepare_conduct_topology;
    let (placements, effective) = compile_inputs();
    let error = prepare_conduct_topology(
        Vec::new(),
        &[],
        &placements,
        &effective,
        &[test_hook(), test_hook()],
        "/work",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("exactly one hook"),
        "got: {error}"
    );
}

#[test]
fn read_write_join_renders_ro_for_retention() {
    // End-to-end intent enforcement (HIGH-2): GuestA Read plus
    // GuestB Write on the same contributed target joins Full
    // grants (Landlock) while the binding renders read-only
    // (VFS) — the join widens the grant without erasing the
    // wall, through merge, render, and revision.
    use cistella::framework::contract::{
        EffectivePermissions, MergeContext, PermissionContribution, Provenance, Rights,
        merge_prepare, render_placements,
    };
    use cistella::framework::landlock_compiler::{
        full_grant_routes, full_grant_sources, revise_ro_for_confinement,
    };
    use cistella::mount::Placement;
    use std::collections::HashSet;
    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("state")).expect("state");
    let host = tree.path().to_string_lossy().to_string();
    let plan = cistella::framework::contract::PreparePlan {
        mounts: vec![cistella::framework::contract::MountContribution {
            placement: Placement {
                host_source: format!("{host}/state"),
                container_target: "/state".to_string(),
            },
            provenance: Provenance::Extension("guest-a".to_string()),
        }],
        permissions: vec![
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Read,
                provenance: Provenance::Extension("guest-a".to_string()),
            },
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Write,
                provenance: Provenance::Extension("guest-b".to_string()),
            },
        ],
        ..cistella::framework::contract::PreparePlan::default()
    };
    let context = MergeContext::new("/home/cistella", HashSet::new(), Vec::new());
    let merged = merge_prepare(
        plan,
        &cistella::framework::contract::CapabilitySet::new(&[
            cistella::framework::contract::Capability::Mounts,
            cistella::framework::contract::Capability::Permissions,
        ]),
        &context,
    )
    .expect("join admits");
    let grant = &merged.permissions[0];
    assert_eq!(grant.rights, Rights::Full, "grants join to Full");
    assert!(grant.binding_intent_ro, "Read intent survives the join");
    let effective = EffectivePermissions::assemble(
        &[],
        &merged.placements,
        &[
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Read,
                provenance: Provenance::Extension("guest-a".to_string()),
            },
            PermissionContribution {
                target: "/state".to_string(),
                rights: Rights::Write,
                provenance: Provenance::Extension("guest-b".to_string()),
            },
        ],
        &[],
    )
    .expect("reassembles");
    let rendered = render_placements(&merged.placements, &effective);
    assert_eq!(
        rendered[0].mode,
        cistella::mount::MountMode::Ro,
        "intent renders read-only despite Full grants"
    );
    let placements = vec![Placement {
        host_source: format!("{host}/state"),
        container_target: "/state".to_string(),
    }];
    let hook = singleton_hook(vec!["/run/cistella/hooks/supervisor-landlock".to_string()]);
    let routes = full_grant_routes(
        &[hook],
        &placements,
        &effective,
        &tree.path().join("state"),
        &tree.path().join("state"),
    )
    .expect("routes derive");
    assert!(
        routes.contains(&"/state".to_string()),
        "Full carveout routes the joined target: {routes:?}"
    );
    let sources = full_grant_sources(&placements, &effective, tree.path());
    assert!(
        sources.contains(&tree.path().join("state")),
        "Full grants join FULL-source accounting: {sources:?}"
    );
    let revised = revise_ro_for_confinement(&rendered, &["/state".to_string()], &sources);
    assert_eq!(
        revised[0].mode,
        cistella::mount::MountMode::Ro,
        "retention keeps the read-only binding under FULL"
    );
    // File case: a proven non-directory with Full rights renders
    // read-write (files carry no Landlock rules; Podman owns
    // them) — intent stays writable, nothing retained.
    std::fs::write(tree.path().join("sock"), b"x").expect("sock");
    let file_placement = Placement {
        host_source: tree.path().join("sock").to_string_lossy().to_string(),
        container_target: "/run/agent.sock".to_string(),
    };
    let file_effective = EffectivePermissions::assemble(
        &[],
        std::slice::from_ref(&file_placement),
        &[PermissionContribution {
            target: "/run/agent.sock".to_string(),
            rights: Rights::Full,
            provenance: Provenance::Extension("probe".to_string()),
        }],
        &[],
    )
    .expect("assembles");
    let file_rendered = render_placements(std::slice::from_ref(&file_placement), &file_effective);
    assert_eq!(
        file_rendered[0].mode,
        cistella::mount::MountMode::Rw,
        "file with Full renders read-write"
    );
}

fn freeze_with(
    placements: &[cistella::mount::Placement],
    hook: &cistella::framework::contract::GuestHookRequest,
) -> String {
    use cistella::framework::contract::{
        EffectivePermissions, PermissionContribution, Provenance, Rights,
    };
    use cistella::framework::landlock_compiler::freeze_snapshot;
    let permissions: Vec<PermissionContribution> = placements
        .iter()
        .map(|placement| PermissionContribution {
            target: placement.container_target.clone(),
            rights: Rights::Read,
            provenance: Provenance::Extension("probe".to_string()),
        })
        .collect();
    let effective =
        EffectivePermissions::assemble(&[], placements, &permissions, &[]).expect("assembles");
    let triples: Vec<cistella::mount::MountTriple> = placements
        .iter()
        .map(|placement| cistella::mount::MountTriple {
            host_source: placement.host_source.clone(),
            container_target: placement.container_target.clone(),
            mode: cistella::mount::MountMode::Ro,
        })
        .collect();
    freeze_snapshot(
        placements,
        &triples,
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        hook,
    )
    .expect("freezes")
    .fingerprint
}

#[test]
fn fingerprint_distinguishes_sources_and_grants() {
    // Same targets backed by different host dirs, and same
    // bindings under different rights, fingerprint differently:
    // a rendering computed from any other snapshot refuses.
    use cistella::mount::Placement;
    let hook = test_hook();
    let base = vec![Placement {
        host_source: "/srv/a".to_string(),
        container_target: "/data".to_string(),
    }];
    let resourced = vec![Placement {
        host_source: "/srv/b".to_string(),
        container_target: "/data".to_string(),
    }];
    assert_ne!(
        freeze_with(&base, &hook),
        freeze_with(&resourced, &hook),
        "different host sources fingerprint differently"
    );
}

#[test]
fn compiled_coverage_refuses_added_binding() {
    // Copied fingerprint with an added output binding: the
    // target-multiset check refuses despite matching strings.
    use cistella::framework::hooks::verify_compiled_binding;
    use cistella::framework::landlock_compiler::{compile_topology, freeze_snapshot};
    let (placements, effective) = compile_inputs();
    let hook = test_hook();
    let snapshot = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .expect("freezes");
    let mut compiled = compile_topology(&snapshot, &hook).expect("compiles");
    compiled.revised.push(cistella::mount::MountTriple {
        host_source: "/srv/evil".to_string(),
        container_target: "/evil".to_string(),
        mode: cistella::mount::MountMode::Rw,
    });
    let error = verify_compiled_binding(&snapshot, &compiled, &hook).unwrap_err();
    assert!(
        error.to_string().contains("coverage"),
        "added output refuses, got: {error}"
    );
}

#[test]
fn compiled_coverage_refuses_missing_binding() {
    use cistella::framework::hooks::verify_compiled_binding;
    use cistella::framework::landlock_compiler::{compile_topology, freeze_snapshot};
    let (placements, effective) = compile_inputs();
    let hook = test_hook();
    let snapshot = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .expect("freezes");
    let mut compiled = compile_topology(&snapshot, &hook).expect("compiles");
    compiled.revised.pop();
    let error = verify_compiled_binding(&snapshot, &compiled, &hook).unwrap_err();
    assert!(
        error.to_string().contains("coverage"),
        "missing output refuses, got: {error}"
    );
}

#[test]
fn freeze_refuses_unregistered_renderer() {
    // Owner dispatch: a hook naming a foreign artifact selects
    // no compiler — refusal names the missing registration
    // instead of silently borrowing landlock's renderer.
    use cistella::framework::landlock_compiler::freeze_snapshot;
    let (placements, effective) = compile_inputs();
    let mut hook = test_hook();
    hook.artifact.source.registry = "third-party".to_string();
    hook.artifact.source.path = "foreign-supervisor".to_string();
    let error = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("no compiler registered"),
        "got: {error}"
    );
}

#[test]
fn compiled_binding_refuses_foreign_owner() {
    use cistella::framework::hooks::verify_compiled_binding;
    use cistella::framework::landlock_compiler::{compile_topology, freeze_snapshot};
    let (placements, effective) = compile_inputs();
    let hook = test_hook();
    let snapshot = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .expect("freezes");
    let mut compiled = compile_topology(&snapshot, &hook).expect("compiles");
    compiled.compiler_owner = "foreign".to_string();
    let error = verify_compiled_binding(&snapshot, &compiled, &hook).unwrap_err();
    assert!(
        error.to_string().contains("escapes its snapshot binding"),
        "got: {error}"
    );
}

#[test]
fn verify_refuses_changed_source_at_same_target() {
    // Same targets, one host source swapped: target-only
    // comparison would pass; source identity refuses.
    use cistella::framework::hooks::verify_compiled_binding;
    use cistella::framework::landlock_compiler::{compile_topology, freeze_snapshot};
    let (placements, effective) = compile_inputs();
    let hook = test_hook();
    let snapshot = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .expect("freezes");
    let mut compiled = compile_topology(&snapshot, &hook).expect("compiles");
    compiled.revised[0].host_source = "/srv/foreign".to_string();
    let error = verify_compiled_binding(&snapshot, &compiled, &hook).unwrap_err();
    assert!(
        error.to_string().contains("coverage"),
        "changed source refuses, got: {error}"
    );
}

#[test]
fn verify_refuses_mutated_snapshot_contents() {
    // Snapshot-field mutation with a reused fingerprint:
    // recomputation fails self-consistency before any output
    // comparison runs.
    use cistella::framework::hooks::verify_compiled_binding;
    use cistella::framework::landlock_compiler::{compile_topology, freeze_snapshot};
    let (placements, effective) = compile_inputs();
    let hook = test_hook();
    let snapshot = freeze_snapshot(
        &placements,
        &compile_triples(),
        &effective,
        std::path::Path::new("/home/op/src"),
        std::path::Path::new("/home/op/src/proj"),
        &hook,
    )
    .expect("freezes");
    let compiled = compile_topology(&snapshot, &hook).expect("compiles");
    let mut snapshot = snapshot;
    snapshot.placements[0].host_source = "/srv/foreign".to_string();
    let error = verify_compiled_binding(&snapshot, &compiled, &hook).unwrap_err();
    assert!(
        error.to_string().contains("self-consistency"),
        "mutated snapshot refuses, got: {error}"
    );
}

#[test]
fn fingerprint_distinguishes_delimiter_collision() {
    // Field-boundary ambiguity: paths containing the old `|`
    // delimiter must fingerprint differently across the split
    // (structural JSON encoding, never delimiter inference).
    use cistella::framework::landlock_compiler::freeze_snapshot;
    use cistella::mount::Placement;
    let hook = test_hook();
    let left = vec![Placement {
        host_source: "/srv/a|b".to_string(),
        container_target: "/data".to_string(),
    }];
    let right = vec![Placement {
        host_source: "/srv/a".to_string(),
        container_target: "/b|/data".to_string(),
    }];
    // Both need grants to freeze: build maps per side.
    use cistella::framework::contract::{
        EffectivePermissions, PermissionContribution, Provenance, Rights,
    };
    let perms_for = |placements: &[Placement]| {
        placements
            .iter()
            .map(|placement| PermissionContribution {
                target: placement.container_target.clone(),
                rights: Rights::Read,
                provenance: Provenance::Extension("probe".to_string()),
            })
            .collect::<Vec<_>>()
    };
    let left_effective =
        EffectivePermissions::assemble(&[], &left, &perms_for(&left), &[]).expect("assembles");
    let right_effective =
        EffectivePermissions::assemble(&[], &right, &perms_for(&right), &[]).expect("assembles");
    let left_triples: Vec<cistella::mount::MountTriple> = left
        .iter()
        .map(|placement| cistella::mount::MountTriple {
            host_source: placement.host_source.clone(),
            container_target: placement.container_target.clone(),
            mode: cistella::mount::MountMode::Ro,
        })
        .collect();
    let right_triples: Vec<cistella::mount::MountTriple> = right
        .iter()
        .map(|placement| cistella::mount::MountTriple {
            host_source: placement.host_source.clone(),
            container_target: placement.container_target.clone(),
            mode: cistella::mount::MountMode::Ro,
        })
        .collect();
    let left_fp = freeze_snapshot(
        &left,
        &left_triples,
        &left_effective,
        std::path::Path::new("/"),
        std::path::Path::new("/"),
        &hook,
    )
    .expect("freezes")
    .fingerprint;
    let right_fp = freeze_snapshot(
        &right,
        &right_triples,
        &right_effective,
        std::path::Path::new("/"),
        std::path::Path::new("/"),
        &hook,
    )
    .expect("freezes")
    .fingerprint;
    assert_ne!(
        left_fp, right_fp,
        "delimiter-colliding snapshots fingerprint differently"
    );
}
