//! Grant-derivation unit tests: compose/full-grant reads of the
//! effective-permissions map (split from prepare_hooks at the
//! file-size limit). Placement/rights inputs, map reads, and the
//! Write-normalization pins live here; hook mechanics stay in
//! prepare_hooks.

/// Strips triples to modeless placements for the map-reading grant path.
fn placements_of(triples: &[cistella::mount::MountTriple]) -> Vec<cistella::mount::Placement> {
    triples
        .iter()
        .map(|triple| cistella::mount::Placement {
            host_source: triple.host_source.clone(),
            container_target: triple.container_target.clone(),
        })
        .collect()
}

/// Driver-feed effective map for fixture triples (modes imply policy).
fn fixture_effective(
    triples: &[cistella::mount::MountTriple],
) -> cistella::framework::contract::EffectivePermissions {
    cistella::framework::contract::EffectivePermissions::assemble(triples, &[], &[], &[])
        .expect("driver-only assembly")
}
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

#[test]
fn compose_hook_argv_orders_wrapper_args_then_harness() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    use std::path::Path;
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![MountTriple {
        host_source: "/home/op/src".to_string(),
        container_target: "/src".to_string(),
        mode: MountMode::Rw,
    }];
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let argv = compose_hook_argv(
        &[hook],
        &placements,
        &effective,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &["sh".to_string(), "-c".to_string(), "echo hi".to_string()],
    )
    .unwrap();
    // The read-write ancestor binding carves FULL on its own
    // route (declarations authoritative); routes render first,
    // then carveouts, then the harness. The /tmp baseline grants
    // FULL unconditionally (second baseline, fixed position).
    assert_eq!(
        argv,
        vec![
            STAGED_WRAPPER_GUEST_PATH.to_string(),
            "--allow-ro=/".to_string(),
            "--allow-rw=/dev".to_string(),
            "--allow-rw=/tmp".to_string(),
            "--allow-ro=/src".to_string(),
            "--allow-rw=/src/proj".to_string(),
            "--allow-rw=/src".to_string(),
            "--".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            "echo hi".to_string(),
        ]
    );
}

#[test]
fn compose_hook_argv_refuses_count_and_cover() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    use std::path::Path;
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![MountTriple {
        host_source: "/home/op/src".to_string(),
        container_target: "/src".to_string(),
        mode: MountMode::Rw,
    }];
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let error = compose_hook_argv(
        &[],
        &placements,
        &effective,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &[],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("exactly one hook"),
        "got: {error}"
    );
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let error = compose_hook_argv(
        &[hook.clone(), hook],
        &placements,
        &effective,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &[],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("exactly one hook"),
        "got: {error}"
    );
    let placements: Vec<cistella::mount::Placement> = Vec::new();
    let effective = fixture_effective(&[]);
    let error = compose_hook_argv(
        &[singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()])],
        &placements,
        &effective,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("untranslatable"), "got: {error}");
}

#[test]
fn compose_admitted_tmp_slice_grants_full_with_nested_bind() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    // A contributed RW triple targeting /tmp composes a FULL
    // carveout by declared mode (no /tmp-specific rule exists —
    // the generic carveout covers it); a nested user bind keeps
    // its own grant (depth-ordered mounts, union semantics).
    let slice = tempfile::tempdir().expect("slice dir");
    let cache = tempfile::tempdir().expect("cache dir");
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![
        MountTriple {
            host_source: slice.path().to_string_lossy().to_string(),
            container_target: "/tmp".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: cache.path().to_string_lossy().to_string(),
            container_target: "/tmp/cache".to_string(),
            mode: MountMode::Rw,
        },
    ];
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let argv = compose_hook_argv(
        &[hook],
        &placements,
        &effective,
        slice.path(),
        slice.path(),
        &["true".to_string()],
    )
    .unwrap();
    assert!(
        argv.contains(&"--allow-rw=/tmp".to_string()),
        "admitted slice grants full, got: {argv:?}"
    );
    assert!(
        argv.contains(&"--allow-rw=/tmp/cache".to_string()),
        "nested user bind keeps its grant, got: {argv:?}"
    );
}

#[test]
fn compose_tmp_granted_by_baseline() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    // Unconditional baseline: /tmp grants FULL under hook with
    // no triple admitting it (operator direction — admitted by
    // construction, whatever backs it).
    let work = tempfile::tempdir().expect("work dir");
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![MountTriple {
        host_source: work.path().to_string_lossy().to_string(),
        container_target: "/work".to_string(),
        mode: MountMode::Rw,
    }];
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let argv = compose_hook_argv(
        &[hook],
        &placements,
        &effective,
        work.path(),
        work.path(),
        &["true".to_string()],
    )
    .unwrap();
    assert!(
        argv.contains(&"--allow-rw=/tmp".to_string()),
        "baseline grants /tmp with no triple, got: {argv:?}"
    );
}

#[test]
fn compose_declared_mounts_grant_by_mode() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    use std::path::Path;
    // Declared mounts grant by declared mode: the read-write
    // ancestor binding grants full on its own route (declarations
    // are authoritative — a validated RW triple left read-only
    // would fail declared-admissible writes); a user bind under
    // /tmp with RW grants full; a socket file is
    // skipped (cannot root path_beneath); an outside host source
    // bound inside the ancestor route (`/opt/state` at
    // `/src/state`) grants full wherever it sits; a read-write
    // sibling submount under the ancestor grants full as a
    // declared carveout; a read-only submount under the ancestor
    // grants nothing (the ancestor read-execute rule denies).
    let cache = tempfile::tempdir().expect("cache dir");
    let socket = tempfile::NamedTempFile::new().expect("socket stand-in");
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![
        MountTriple {
            host_source: "/home/op/src".to_string(),
            container_target: "/src".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: cache.path().to_string_lossy().to_string(),
            container_target: "/tmp/cache".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: socket.path().to_string_lossy().to_string(),
            container_target: "/run/agent.sock".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: "/home/op/src/other".to_string(),
            container_target: "/other".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: "/opt/state".to_string(),
            container_target: "/src/state".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: "/home/op/src/ro-data".to_string(),
            container_target: "/rodata".to_string(),
            mode: MountMode::Ro,
        },
    ];
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let argv = compose_hook_argv(
        &[hook],
        &placements,
        &effective,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &["true".to_string()],
    )
    .unwrap();
    assert_eq!(
        argv,
        vec![
            STAGED_WRAPPER_GUEST_PATH.to_string(),
            "--allow-ro=/".to_string(),
            "--allow-rw=/dev".to_string(),
            "--allow-rw=/tmp".to_string(),
            "--allow-ro=/src".to_string(),
            "--allow-rw=/src/proj".to_string(),
            "--allow-rw=/src".to_string(),
            "--allow-rw=/tmp/cache".to_string(),
            "--allow-rw=/other".to_string(),
            "--allow-rw=/src/state".to_string(),
            "--allow-ro=/rodata".to_string(),
            "--".to_string(),
            "true".to_string(),
        ]
    );
}

#[test]
fn full_grant_routes_mirror_compose_carveouts() {
    use cistella::framework::prepare::full_grant_routes;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    use std::path::Path;
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![
        MountTriple {
            host_source: "/home/op/src".to_string(),
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: "/home/op/src/proj".to_string(),
            container_target: "/src/proj".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: "/opt/state".to_string(),
            container_target: "/src/state".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: "/tmp/cache".to_string(),
            container_target: "/tmp/cache".to_string(),
            mode: MountMode::Rw,
        },
    ];
    // Fixed FULL baselines (now including /tmp), subtree route,
    // plus the RW carveout targets (project graft, outside
    // source bound inside the ancestor route, and uncovered
    // user bind); the RO ancestor is not FULL.
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let full = full_grant_routes(
        &[hook],
        &placements,
        &effective,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
    )
    .unwrap();
    assert_eq!(
        full,
        vec![
            "/dev".to_string(),
            "/tmp".to_string(),
            "/src/proj".to_string(),
            "/src/state".to_string(),
            "/tmp/cache".to_string()
        ]
    );
}

#[test]
fn compose_canonicalizes_target_spellings_for_coverage() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    use std::path::Path;
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    // Traversal, duplicate-slash, and dot-segment spellings of
    // targets under the FULL subtree: grant computation runs on
    // canonical forms, so the RO alias is covered (no stray
    // subtract-incapable RO rule) and the RW carveout emits
    // canonical.
    let triples = vec![
        MountTriple {
            host_source: "/home/op/src".to_string(),
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: "/home/op/src/proj".to_string(),
            container_target: "/src/proj".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: "/home/op/src/proj/ro-data".to_string(),
            container_target: "/src/../src/proj/ro-data".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: "/home/op/src/g".to_string(),
            container_target: "/src//g/./x".to_string(),
            mode: MountMode::Rw,
        },
    ];
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let argv = compose_hook_argv(
        &[hook],
        &placements,
        &effective,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &[],
    )
    .unwrap();
    assert!(
        !argv
            .iter()
            .any(|flag| flag.contains("..") || flag.contains("//")),
        "no raw alias spelling reaches argv: {argv:?}"
    );
    assert!(
        !argv.contains(&"--allow-ro=/src/proj/ro-data".to_string()),
        "covered RO alias emits no rule: {argv:?}"
    );
    assert!(
        argv.contains(&"--allow-rw=/src/g/x".to_string()),
        "RW carveout emits canonical: {argv:?}"
    );
}

#[test]
fn compose_skips_proven_nondirectories_but_grants_missing() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    let dir = tempfile::tempdir().expect("tmpdir");
    let host = dir.path().to_string_lossy().to_string();
    let subtree = dir.path().join("proj");
    std::fs::create_dir_all(&subtree).expect("proj dir");
    std::fs::create_dir_all(dir.path().join("data")).expect("data dir");
    // Proven non-directories: a file and a bound socket. A
    // socket fed to the wrapper fails apply (O_DIRECTORY on
    // a non-directory is ENOTDIR) and refuses the whole
    // session — proven live by a credential-surface socket.
    std::fs::write(dir.path().join("agent-file"), b"x").expect("agent file");
    let _socket = UnixListener::bind(dir.path().join("agent.sock")).expect("agent socket");
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![
        MountTriple {
            host_source: host.clone(),
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: subtree.to_string_lossy().to_string(),
            container_target: "/src/proj".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/agent-file"),
            container_target: "/opt/agent-file".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: format!("{host}/agent.sock"),
            container_target: "/run/sock".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/agent.sock"),
            container_target: "/run/rosock".to_string(),
            mode: MountMode::Ro,
        },
        // Not-yet-existing paths still grant by mode (a
        // wrong-kind materialization fails loudly at apply).
        MountTriple {
            host_source: format!("{host}/missing"),
            container_target: "/opt/missing".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: format!("{host}/data"),
            container_target: "/data".to_string(),
            mode: MountMode::Rw,
        },
    ];
    let placements = placements_of(&triples);
    let effective = fixture_effective(&triples);
    let argv = compose_hook_argv(
        &[hook],
        &placements,
        &effective,
        Path::new(&host),
        &subtree,
        &["sleep".to_string(), "infinity".to_string()],
    )
    .unwrap();
    let text = argv.join("\n");
    for banned in ["/opt/agent-file", "/run/sock", "/run/rosock"] {
        assert!(
            !text.contains(banned),
            "proven non-directory {banned} must not reach allow flags: {text}"
        );
    }
    for granted in ["/src/proj", "/opt/missing", "/data"] {
        assert!(
            text.contains(&format!("--allow-rw={granted}")),
            "granted route {granted} missing: {text}"
        );
    }
}
