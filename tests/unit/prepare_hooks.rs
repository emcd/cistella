//! Extension prepare + hook gate unit tests (split from policy_prepare at the file-size limit).
//!
//! Landlock extension prepare over a staged fake peer, hook
//! executable binding, probe/attestation strict shapes,
//! confinement roots, argv composition, and the attestation-gate
//! drain matrix. Lattice and merge-transaction coverage stays in
//! policy_prepare.

use std::collections::HashMap;
use std::path::PathBuf;

use cistella::framework::contract::GuestHookRequest;
use cistella::framework::hooks::gate_hook_attestation;
use cistella::framework::policy::PolicySet;
use cistella::framework::prepare::{EXTENSION_BIN, check_hook_executable, run_landlock_prepare};
use cistella::profile::{CredentialSurface, Profile};

/// Compiled defaults via load: an absent file means defaults only.
fn defaults() -> PolicySet {
    let dir = tempfile::tempdir().expect("tempdir");
    PolicySet::load(Some(dir.path())).expect("absent file means defaults")
}

fn fake_guest_path() -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let profile_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors");
    let examples_dir = profile_dir.join("examples");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&examples_dir) {
        for entry in entries {
            let entry = entry.expect("dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("fake_guest") {
                continue;
            }
            let after = &name["fake_guest".len()..];
            if !after.is_empty() && !after.starts_with('-') {
                continue;
            }
            let metadata = entry.metadata().expect("metadata");
            if !metadata.is_file() || (metadata.mode() & 0o111) == 0 {
                continue;
            }
            candidates.push((metadata.modified().expect("mtime"), entry.path()));
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .unwrap_or_else(|| {
            panic!(
                "fake_guest executable not found in {}; run `cargo build --examples` first",
                examples_dir.display()
            )
        })
}

/// Stages the fake guest under the extension binary name in a
/// scratch dir: discovery pins the bare name, never PATH.
fn stage_extension_peer() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let staged = dir.path().join(EXTENSION_BIN);
    std::fs::copy(fake_guest_path(), &staged).expect("stage peer");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&staged).expect("meta").permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(&staged, permissions).expect("exec bit");
    dir
}

fn bare_profile() -> Profile {
    Profile {
        image: "localhost/cistella-test:latest".to_string(),
        mounts: Vec::new(),
        command: None,
        environment_assignments: HashMap::new(),
        environment_acceptances: Vec::new(),
        credential_surface: CredentialSurface::None,
        container_home: "/home/cistella".to_string(),
        labels: HashMap::new(),
        isolator: cistella::profile::IsolatorConfig {
            name: "podman".to_string(),
        },
        extensions: vec![cistella::profile::ExtensionConfig {
            name: "landlock".to_string(),
        }],
    }
}

#[test]
fn extension_prepare_merges_hook_and_shuts_down_clean() {
    let dir = stage_extension_peer();
    let mode = "--mode=extension-landlock-hook".to_string();
    // `Ok` already proves clean shutdown: residue dominates inside
    // `run_landlock_prepare`, so a stranded peer would fail here.
    let evaluated =
        run_landlock_prepare(dir.path(), &[mode], &bare_profile(), &[], &defaults()).unwrap();
    assert_eq!(evaluated.merged.guest_hooks.len(), 1);
    let hook = &evaluated.merged.guest_hooks[0];
    assert_eq!(hook.order, 0);
    assert_eq!(
        hook.argv_prefix,
        vec!["/run/cistella/hooks/landlock-wrap".to_string()]
    );
    assert!(evaluated.merged.environment.is_empty());
    assert!(evaluated.merged.mounts.is_empty());
}

#[test]
fn extension_prepare_without_hooks_capability_refuses() {
    let dir = stage_extension_peer();
    // Role-only hello passes closed negotiation but lacks the
    // contribution: admission refuses before any prepare is sent,
    // and the guest is reaped (Ok shutdown inside is residue-gated).
    let mode = "--mode=extension-no-hooks".to_string();
    let error =
        run_landlock_prepare(dir.path(), &[mode], &bare_profile(), &[], &defaults()).unwrap_err();
    assert!(
        error.to_string().contains("must advertise guest-hooks"),
        "got: {error}"
    );
}

fn singleton_hook(prefix: Vec<String>) -> GuestHookRequest {
    use cistella::framework::contract::{HookArtifact, HookProbe, HookSource};
    GuestHookRequest {
        artifact: HookArtifact {
            kind: "digest-pinned-blob".to_string(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            source: HookSource {
                registry: "shipped".to_string(),
                path: "cistella-landlock-wrap".to_string(),
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
fn hook_executable_binding_accepts_staged_singleton() {
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    check_hook_executable(&hook).unwrap();
}

#[test]
fn hook_executable_binding_refuses_shell() {
    // Correctly pinned artifact, hostile executable: the digest
    // bind is independent of the composition target.
    let hook = singleton_hook(vec!["/bin/sh".to_string()]);
    let error = check_hook_executable(&hook).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("exactly the staged wrapper path"),
        "got: {error}"
    );
}

#[test]
fn hook_executable_binding_refuses_extension_args() {
    // The prepare payload carries no session context, so
    // session-blind extension args are never legitimate: the
    // framework composes all wrapper arguments at launch.
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    let hook = singleton_hook(vec![
        STAGED_WRAPPER_GUEST_PATH.to_string(),
        "--allow-raw-io".to_string(),
    ]);
    let error = check_hook_executable(&hook).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("exactly the staged wrapper path"),
        "got: {error}"
    );
}

#[test]
fn probe_report_gates_abi_and_mask() {
    use cistella::framework::prepare::parse_probe_report;
    parse_probe_report(br#"{"abi":7,"handled_fs_mask":32767}"#).unwrap();
    let error = parse_probe_report(br#"{"abi":2,"handled_fs_mask":32767}"#).unwrap_err();
    assert!(error.to_string().contains("below minimum"), "got: {error}");
    let error = parse_probe_report(br#"{"abi":7,"handled_fs_mask":8191}"#).unwrap_err();
    assert!(
        error.to_string().contains("rights shortfall"),
        "got: {error}"
    );
    let error =
        parse_probe_report(br#"{"unsupported":"landlock_create_ruleset: ENOSYS"}"#).unwrap_err();
    assert!(error.to_string().contains("unsupported"), "got: {error}");
    let error = parse_probe_report(br#"{"abi":"seven"}"#).unwrap_err();
    assert!(error.to_string().contains("shape"), "got: {error}");
}

#[test]
fn attestation_line_parses_applied() {
    use cistella::framework::prepare::parse_attestation_line;
    assert_eq!(
        parse_attestation_line(r#"{"applied":true,"abi":7,"handled_fs_mask":32767}"#).unwrap(),
        (7, 32767)
    );
    let error =
        parse_attestation_line(r#"{"applied":false,"error":"bad allow path"}"#).unwrap_err();
    assert!(error.to_string().contains("apply failure"), "got: {error}");
    let error = parse_attestation_line("not json").unwrap_err();
    assert!(error.to_string().contains("shape"), "got: {error}");
}

#[test]
fn confinement_roots_require_src_tree() {
    use cistella::framework::prepare::confinement_roots;
    use std::path::Path;
    let (ancestor, subtree) =
        confinement_roots(Path::new("/home/op"), "/home/op/src/proj").unwrap();
    assert_eq!(ancestor, Path::new("/home/op/src"));
    assert_eq!(subtree, Path::new("/home/op/src/proj"));
    let error = confinement_roots(Path::new("/home/op"), "/srv/other").unwrap_err();
    assert!(
        error.to_string().contains("confinement root"),
        "got: {error}"
    );
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
    let argv = compose_hook_argv(
        &[hook],
        &triples,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &["sh".to_string(), "-c".to_string(), "echo hi".to_string()],
    )
    .unwrap();
    // The read-write ancestor binding carves FULL on its own
    // route (declarations authoritative); routes render first,
    // then carveouts, then the harness.
    assert_eq!(
        argv,
        vec![
            STAGED_WRAPPER_GUEST_PATH.to_string(),
            "--allow-ro=/".to_string(),
            "--allow-rw=/dev".to_string(),
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
    let error = compose_hook_argv(
        &[],
        &triples,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &[],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("exactly one hook"),
        "got: {error}"
    );
    let error = compose_hook_argv(
        &[hook.clone(), hook],
        &triples,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &[],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("exactly one hook"),
        "got: {error}"
    );
    let error = compose_hook_argv(
        &[singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()])],
        &[],
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("untranslatable"), "got: {error}");
}

#[test]
fn attestation_strict_schema_refuses_dups_and_extras() {
    use cistella::framework::prepare::parse_attestation_line;
    // Duplicate keys refuse (derived Deserialize rejects them).
    let error =
        parse_attestation_line(r#"{"applied":true,"abi":7,"handled_fs_mask":32767,"abi":8}"#)
            .unwrap_err();
    assert!(error.to_string().contains("shape"), "got: {error}");
    // Unknown fields refuse.
    let error = parse_attestation_line(
        r#"{"applied":true,"abi":7,"handled_fs_mask":32767,"harness_pid":123}"#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("shape"), "got: {error}");
    // Cross-shape mismatch refuses (applied:true must not carry error).
    let error =
        parse_attestation_line(r#"{"applied":true,"abi":7,"handled_fs_mask":32767,"error":"x"}"#)
            .unwrap_err();
    assert!(error.to_string().contains("shape"), "got: {error}");
    // Negative shape still reports.
    let error =
        parse_attestation_line(r#"{"applied":false,"error":"bad allow path"}"#).unwrap_err();
    assert!(error.to_string().contains("apply failure"), "got: {error}");
}

#[test]
fn probe_strict_schema_refuses_extras() {
    use cistella::framework::prepare::parse_probe_report;
    let error = parse_probe_report(br#"{"abi":7,"handled_fs_mask":32767,"extra":1}"#).unwrap_err();
    assert!(error.to_string().contains("shape"), "got: {error}");
}

#[test]
fn gate_drain_captures_exec_failure_and_ignores_noise() {
    use std::os::fd::AsFd;
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    let script = concat!(
        "{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n",
        "{\"transitioned\":false,\"error\":\"exec failed: ENOENT\"}\n",
    );
    use std::io::Write;
    write.write_all(script.as_bytes()).expect("write script");
    drop(write);
    let (abi, detail) =
        gate_hook_attestation(&read, Duration::from_secs(5)).expect("gate passes on attestation");
    assert_eq!(abi, 7);
    assert_eq!(detail.as_deref(), Some("exec failed: ENOENT"));
    let _ = read.as_fd();
}

#[test]
fn gate_drain_clean_session_has_no_detail() {
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    write
        .write_all(
            b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n{\"transitioned\":true}\n",
        )
        .expect("write script");
    drop(write);
    let (abi, detail) = gate_hook_attestation(&read, Duration::from_secs(5)).expect("gate passes");
    assert_eq!(abi, 7);
    assert_eq!(detail, None);
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
    // would fail declared-admissible writes); scratch outside the
    // ancestor domain with RW grants full; a socket file is
    // skipped (cannot root path_beneath); an outside host source
    // bound inside the ancestor route (`/opt/state` at
    // `/src/state`) grants full wherever it sits; a read-write
    // sibling submount under the ancestor grants full as a
    // declared carveout; a read-only submount under the ancestor
    // grants nothing (the ancestor read-execute rule denies).
    let scratch = tempfile::tempdir().expect("scratch dir");
    let socket = tempfile::NamedTempFile::new().expect("socket stand-in");
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let triples = vec![
        MountTriple {
            host_source: "/home/op/src".to_string(),
            container_target: "/src".to_string(),
            mode: MountMode::Rw,
        },
        MountTriple {
            host_source: scratch.path().to_string_lossy().to_string(),
            container_target: "/tmp/scratch".to_string(),
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
    let argv = compose_hook_argv(
        &[hook],
        &triples,
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
            "--allow-ro=/src".to_string(),
            "--allow-rw=/src/proj".to_string(),
            "--allow-rw=/src".to_string(),
            "--allow-rw=/tmp/scratch".to_string(),
            "--allow-rw=/other".to_string(),
            "--allow-rw=/src/state".to_string(),
            "--allow-ro=/rodata".to_string(),
            "--".to_string(),
            "true".to_string(),
        ]
    );
}

#[test]
fn gate_drain_malformed_trailing_refuses() {
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    write
        .write_all(b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\nnot-json-noise\n")
        .expect("write script");
    drop(write);
    let error = gate_hook_attestation(&read, Duration::from_secs(5)).unwrap_err();
    assert!(
        error.to_string().contains("malformed diagnostics trailing"),
        "got: {error}"
    );
}

#[test]
fn gate_drain_missing_eof_times_out_typed() {
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    // Attestation only, write end HELD OPEN (no EOF): the gate
    // must not classify the launch successful without the
    // exec-seal EOF. Short deadline keeps the pin fast.
    write
        .write_all(b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n")
        .expect("write script");
    write.flush().expect("flush");
    let error = gate_hook_attestation(&read, Duration::from_millis(300)).unwrap_err();
    assert!(error.to_string().contains("timed out"), "got: {error}");
    drop(write);
}

#[test]
fn gate_drain_overlong_refuses() {
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    // 70 KiB of newline-free bytes from a thread (pipe buffer
    // would block a single-threaded writer past 64 KiB).
    let filler = vec![b'x'; 70 * 1024];
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let _ = write.write_all(b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n");
        let _ = write.write_all(&filler);
    });
    let error = gate_hook_attestation(&read, Duration::from_secs(10)).unwrap_err();
    assert!(error.to_string().contains("overlong"), "got: {error}");
    let _ = writer.join();
}

#[test]
fn gate_drain_eof_without_transition_refuses() {
    use std::time::Duration;
    // Attestation then EOF with NO transitioned line (crash
    // between seal and exec): must not classify as success.
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    write
        .write_all(b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n")
        .expect("write script");
    drop(write);
    let error = gate_hook_attestation(&read, Duration::from_secs(5)).unwrap_err();
    assert!(
        error.to_string().contains("transition unproven"),
        "got: {error}"
    );
}

#[test]
fn gate_drain_transition_failure_reports_detail() {
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    write
        .write_all(
            b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n{\"transitioned\":false,\"error\":\"child died\"}\n",
        )
        .expect("write script");
    drop(write);
    let (abi, detail) =
        gate_hook_attestation(&read, Duration::from_secs(5)).expect("gate reads failure");
    assert_eq!(abi, 7);
    assert_eq!(detail.as_deref(), Some("child died"));
}

#[test]
fn gate_drain_ambiguous_signal_reports_transition_ambiguity() {
    use std::time::Duration;
    // The ambiguous race-window shape (indistinguishable
    // pre-exec wrapper death vs early harness signal) classifies
    // as transition ambiguity with cause — neither wrapper
    // failure nor harness outcome, never silent.
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    write
        .write_all(
            b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n{\"transitioned\":false,\"error\":\"transition ambiguous: signal SIGTERM\"}\n",
        )
        .expect("write script");
    drop(write);
    let (abi, detail) =
        gate_hook_attestation(&read, Duration::from_secs(5)).expect("gate reads failure");
    assert_eq!(abi, 7);
    assert_eq!(
        detail.as_deref(),
        Some("transition ambiguous: signal SIGTERM")
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
            host_source: "/tmp/scratch".to_string(),
            container_target: "/tmp/scratch".to_string(),
            mode: MountMode::Rw,
        },
    ];
    // Fixed FULL baselines, subtree route, plus the RW
    // carveout targets (project graft, outside source bound
    // inside the ancestor route, and uncovered scratch);
    // the RO ancestor is not FULL.
    let full = full_grant_routes(
        &[hook],
        &triples,
        Path::new("/home/op/src"),
        Path::new("/home/op/src/proj"),
    )
    .unwrap();
    assert_eq!(
        full,
        vec![
            "/dev".to_string(),
            "/src/proj".to_string(),
            "/src/state".to_string(),
            "/tmp/scratch".to_string()
        ]
    );
}

#[test]
fn refuse_extension_rw_mounts_gates_carveout_admission() {
    use cistella::framework::prepare::refuse_extension_rw_mounts;
    use cistella::mount::{MountMode, MountTriple};
    let rw = MountTriple {
        host_source: "/srv/data".to_string(),
        container_target: "/data".to_string(),
        mode: MountMode::Rw,
    };
    let ro = MountTriple {
        host_source: "/run/vector/agentmux-bus".to_string(),
        container_target: "/run/vector/agentmux-bus".to_string(),
        mode: MountMode::Ro,
    };
    // An RW triple refuses (it would compose into a FULL
    // carveout); RO contributions still merge (vectors pin the
    // bus-socket shape).
    let error = refuse_extension_rw_mounts(&[ro.clone(), rw]).unwrap_err();
    assert!(error.to_string().contains("not admitted"), "got: {error}");
    refuse_extension_rw_mounts(&[]).expect("empty passes");
    refuse_extension_rw_mounts(&[ro]).expect("read-only passes");
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
    let argv = compose_hook_argv(
        &[hook],
        &triples,
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
fn require_declared_hooks_refuses_selected_but_empty() {
    use cistella::framework::hooks::require_declared_hooks;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    // Selected but answered nothing (malformed/buggy/replaced
    // guest): refuse pre-create rather than start plain and
    // unconfined silently.
    let error = require_declared_hooks(true, &[]).unwrap_err();
    assert!(error.to_string().contains("no hook"), "got: {error}");
    // Selected with hooks: pass (multiplicity is composition's
    // pre-create refusal, not this gate's).
    require_declared_hooks(true, &[hook.clone(), hook.clone()]).expect("multi passes here");
    require_declared_hooks(true, std::slice::from_ref(&hook)).expect("one passes");
    // Undeclared: nothing promised, nothing enforced.
    require_declared_hooks(false, &[]).expect("undeclared empty passes");
}

#[test]
fn compose_skips_proven_nondirectories_but_grants_missing() {
    use cistella::framework::prepare::compose_hook_argv;
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    use cistella::mount::{MountMode, MountTriple};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    let dir = tempfile::tempdir().expect("tempdir");
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
    let argv = compose_hook_argv(
        &[hook],
        &triples,
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
