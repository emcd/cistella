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
use cistella::framework::prepare::{EXTENSION_BIN, check_hook_executable};
use cistella::framework::prepare_select::run_landlock_prepare;
use cistella::profile::{CredentialSurface, Profile};

/// Compiled defaults via load: an absent file means defaults only.
fn defaults() -> PolicySet {
    let dir = tempfile::tempdir().expect("tmpdir");
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
/// temp dir: discovery pins the bare name, never PATH.
fn stage_extension_peer() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tmpdir");
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
        permissions: Vec::new(),
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
    let (evaluated, _lease) = run_landlock_prepare(
        dir.path(),
        &[mode],
        &bare_profile(),
        &[],
        &defaults(),
        "test-session",
        dir.path(),
    )
    .unwrap();
    assert_eq!(evaluated.merged.guest_hooks.len(), 1);
    let hook = &evaluated.merged.guest_hooks[0];
    assert_eq!(hook.order, 0);
    assert_eq!(
        hook.argv_prefix,
        vec!["/run/cistella/hooks/supervisor-landlock".to_string()]
    );
    assert!(evaluated.merged.environment.is_empty());
    assert!(evaluated.merged.placements.is_empty());
}

#[test]
fn extension_prepare_without_hooks_capability_refuses() {
    let dir = stage_extension_peer();
    // Role-only hello passes closed negotiation but lacks the
    // contribution: admission refuses before any prepare is sent,
    // and the guest is reaped (Ok shutdown inside is residue-gated).
    let mode = "--mode=extension-no-hooks".to_string();
    let error = run_landlock_prepare(
        dir.path(),
        &[mode],
        &bare_profile(),
        &[],
        &defaults(),
        "test-session",
        dir.path(),
    )
    .unwrap_err();
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
    // transition line. Short deadline keeps the pin fast.
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
fn gate_drain_transitioned_returns_before_eof() {
    use std::time::{Duration, Instant};
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    // Attestation plus the positive transition line, write end
    // HELD OPEN (no EOF): the gate must pass on the proof
    // itself — a podman forwarding hold keeps EOF until
    // session end, and waiting for it capped harness lifetime
    // at the pre-execute deadline (proven live: 50s harness
    // dying at the 30s drain).
    write
        .write_all(
            b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n{\"transitioned\":true}\n",
        )
        .expect("write script");
    write.flush().expect("flush");
    let start = Instant::now();
    let (abi, detail) = gate_hook_attestation(&read, Duration::from_secs(30)).expect("gate passes");
    assert_eq!(abi, 7);
    assert_eq!(detail, None);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "gate must not wait for EOF"
    );
    drop(write);
}

#[test]
fn gate_drain_transitioned_with_trailing_bytes_refuses() {
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut write: std::fs::File = write.into();
    use std::io::Write;
    // Positive line plus already-buffered contradiction and
    // malformed tail in the SAME write: neither may ride a
    // success — the gate refuses typed on bytes available
    // before it returns (later bytes need a separate reader).
    write
        .write_all(
            b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n{\"transitioned\":true}\n{\"transitioned\":false,\"error\":\"exec failed\"}\nnot-json-noise\n",
        )
        .expect("write script");
    drop(write);
    let error = gate_hook_attestation(&read, Duration::from_secs(5)).unwrap_err();
    assert!(
        error.to_string().contains("malformed diagnostics trailing"),
        "got: {error}"
    );
}

#[test]
fn gate_drain_transitioned_with_overcap_tail_refuses() {
    use std::time::Duration;
    let (read, write) = nix::unistd::pipe().expect("pipe");
    // Positive line with a 70 KiB buffered tail behind it: the
    // tail refuses as malformed trailing (the per-pass ceiling
    // guards lineless streams; a transitioned line with any
    // buffered tail refuses here). The writer runs threaded
    // and ignores write errors: 70 KiB exceeds the pipe
    // buffer, and after the gate refuses, the dropped read
    // end turns the remainder into EPIPE — both fine.
    let writer = std::thread::spawn(move || {
        let mut write: std::fs::File = write.into();
        use std::io::Write;
        let mut script =
            b"{\"applied\":true,\"abi\":7,\"handled_fs_mask\":32767}\n{\"transitioned\":true}\n"
                .to_vec();
        script.extend(std::iter::repeat_n(b'x', 70 * 1024));
        let _ = write.write_all(&script);
    });
    let error = gate_hook_attestation(&read, Duration::from_secs(5)).unwrap_err();
    assert!(
        error.to_string().contains("malformed diagnostics trailing"),
        "got: {error}"
    );
    drop(read);
    writer.join().expect("writer joins");
}
