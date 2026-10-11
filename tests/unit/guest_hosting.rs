//! Registered-guest hosting: verified-FD spawn, closed hello, and
//! the pre-planning role compare.
//!
//! The deterministic peer (`fake_guest`) stands in for a registered
//! binary: entries point at it by path with a freshly computed
//! digest, so every test exercises the real resolve-then-host path
//! (no stubs, no unwired seams).

use cistella::framework::contract::Deadlines;
use cistella::framework::guest::host_registered;
use cistella::guest_registry::{Guest, SourceLocation};
use cistella::guest_source::{ResolveCtx, resolve};

/// Resolves a built example binary (newest-mtime executable match).
///
/// Requires `cargo build --examples` first for filtered runs
/// (`cargo test --test unit` does not build examples; full
/// `cargo nextest run` does via the default target set).
fn example_binary(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let examples_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors")
        .join("examples");
    let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&examples_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name != name && !file_name.starts_with(&format!("{name}-")) {
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
                "example binary {name} not found in {}; run `cargo build --examples` first",
                examples_dir.display()
            )
        })
}

fn short_deadlines() -> Deadlines {
    Deadlines {
        hello: std::time::Duration::from_secs(5),
        plan: std::time::Duration::from_secs(5),
        apply: std::time::Duration::from_secs(5),
        terminate_grace: std::time::Duration::from_secs(5),
        frame_completion: std::time::Duration::from_secs(60),
    }
}

fn peer_entry(
    binary: &str,
    role: &str,
    arguments: Vec<String>,
) -> (Guest, ResolveCtx, tempfile::TempDir) {
    use sha2::Digest;
    use std::os::unix::fs::PermissionsExt;
    let source = example_binary(binary);
    let bytes = std::fs::read(&source).expect("read peer binary");
    let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
    let dir = tempfile::tempdir().expect("tempdir");
    // Private owned 0755 copy: build-output permissions follow
    // the seat umask (0002 leaves group-writable binaries the
    // ownership gate correctly refuses), so resolution pins its
    // own fixture perms instead of inheriting the build's.
    let path = dir.path().join(binary);
    std::fs::write(&path, &bytes).expect("stage peer copy");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let entry = Guest {
        name: "test-peer".to_string(),
        role: role.to_string(),
        protocol: 1,
        source: SourceLocation::Path {
            source_path: path,
            digest,
        },
        arguments,
    };
    let ctx = ResolveCtx::new(dir.path().join("cache"));
    (entry, ctx, dir)
}

#[test]
fn registered_hello_role_matches() {
    let (entry, ctx, _dir) = peer_entry("fake_guest", "test-cap", Vec::new());
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let offered = ["test-cap".to_string()];
    let (mut host, negotiated) =
        host_registered(resolved, &[], &offered, short_deadlines()).expect("role matches");
    assert_eq!(negotiated, vec!["test-cap".to_string()]);
    host.shutdown().expect("clean shutdown");
}

#[test]
fn role_mismatch_refuses_pre_planning() {
    let (entry, ctx, _dir) = peer_entry("fake_guest", "other-role", Vec::new());
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let offered = ["test-cap".to_string(), "other-role".to_string()];
    let error = match host_registered(resolved, &[], &offered, short_deadlines()) {
        Ok(_) => panic!("wrong role must refuse"),
        Err(error) => error,
    };
    // Names the registry declaration, never guest bytes.
    assert!(
        error.to_string().contains("role mismatch") && error.to_string().contains("test-peer"),
        "got: {error}"
    );
}

#[test]
fn unadvertised_capability_refuses_by_index() {
    let (entry, ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=hello-real-capabilities".to_string()],
    );
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let offered = ["test-cap".to_string()];
    let error = match host_registered(resolved, &[], &offered, short_deadlines()) {
        Ok(_) => panic!("junk capabilities must refuse"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("unsupported guest capability at index"),
        "got: {error}"
    );
}

#[test]
fn example_guest_advertises_configured_role() {
    // Second conformance datapoint beside the peer: the minimal
    // correct-behavior guest, default role plus argv-configured.
    let (entry, ctx, _dir) = peer_entry("example_guest", "example", Vec::new());
    let resolved = resolve(&entry, &ctx).expect("example resolves");
    let offered = ["example".to_string()];
    let (mut host, negotiated) =
        host_registered(resolved, &[], &offered, short_deadlines()).expect("example hosts");
    assert_eq!(negotiated, vec!["example".to_string()]);
    host.shutdown().expect("clean shutdown");
    let (entry, ctx, _dir) = peer_entry(
        "example_guest",
        "custom-role",
        vec!["--role=custom-role".to_string()],
    );
    let resolved = resolve(&entry, &ctx).expect("example resolves");
    let offered = ["custom-role".to_string()];
    let (mut host, negotiated) =
        host_registered(resolved, &[], &offered, short_deadlines()).expect("custom role hosts");
    assert_eq!(negotiated, vec!["custom-role".to_string()]);
    host.shutdown().expect("clean shutdown");
}
#[test]
fn registered_argv_rides_spawn_verbatim() {
    // The mode selector reaches the peer only through the
    // registered argv: the negotiated set proves verbatim delivery.
    let real_caps = [
        "environment",
        "mounts",
        "policy-claims",
        "guest-hooks",
        "credentials",
    ]
    .iter()
    .map(|name| name.to_string())
    .collect::<Vec<_>>();
    let (entry, ctx, _dir) = peer_entry(
        "fake_guest",
        "environment",
        vec!["--mode=hello-real-capabilities".to_string()],
    );
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let (mut host, negotiated) = host_registered(resolved, &[], &real_caps, short_deadlines())
        .expect("registered argv rides");
    assert_eq!(negotiated, real_caps);
    host.shutdown().expect("clean shutdown");
    // Call-site args append after registered argv (peer mode
    // selectors stay scriptable per call).
    let (entry, ctx, _dir) = peer_entry("fake_guest", "environment", Vec::new());
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let (mut host, negotiated) = host_registered(
        resolved,
        &["--mode=hello-real-capabilities".to_string()],
        &real_caps,
        short_deadlines(),
    )
    .expect("call-site argv rides");
    assert_eq!(negotiated, real_caps);
    host.shutdown().expect("clean shutdown");
}

fn minimal_profile() -> cistella::profile::Profile {
    toml::from_str(
        "image = \"localhost/cistella/opencode:example\"\n\
         credential-surface = \"none\"\n\
         mounts = []\n",
    )
    .expect("minimal profile parses")
}

#[test]
fn registered_extension_prepare_converges_plain() {
    use cistella::framework::prepare::{ExtensionSpawn, run_extension_prepare};
    // The example guest answers prepare with zero hooks through
    // the registered path: conduct proceeds on the plain path
    // with empty contributions merged (no refusal; hooks were
    // never promised). Divergence from the shipped-path merge
    // behavior is a conformance failure.
    let (entry, ctx, _dir) = peer_entry("example_guest", "example", Vec::new());
    let resolved = resolve(&entry, &ctx).expect("example resolves");
    let policy_dir = tempfile::tempdir().expect("tempdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let plan = run_extension_prepare(
        ExtensionSpawn::Registered(resolved),
        std::path::Path::new("/nonexistent-exe-dir"),
        &[],
        &minimal_profile(),
        &[],
        &policy,
    )
    .expect("registered prepare converges");
    assert!(plan.merged.environment.is_empty());
    assert!(plan.merged.mounts.is_empty());
    assert!(plan.merged.guest_hooks.is_empty());
}

#[test]
fn registered_isolator_serves_lifecycle() {
    use cistella::framework::contract::{
        CancelFlag, ExecutionHandle, ReconciliationKey, UnitHandle,
    };
    use cistella::framework::isolator::{ExecutionOutcome, Isolator};
    use cistella::isolators::client::WireClient;
    // The scripted peer speaks the isolator wire through the
    // registered path: rendezvous accept, concurrent dispatcher,
    // and op service are identical past hello (same serve tail).
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let (entry, ctx, _dir) = peer_entry(
        "fake_guest",
        "isolator",
        vec![
            "--mode=isolator-concurrent".to_string(),
            format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
        ],
    );
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let mut deadlines = short_deadlines();
    deadlines.apply = std::time::Duration::from_secs(15);
    let client = std::sync::Arc::new(
        WireClient::host_registered(resolved, rendezvous.path(), deadlines)
            .expect("registered isolator hosts"),
    );
    let execution = ExecutionHandle::mint();
    let waiter = {
        let client = std::sync::Arc::clone(&client);
        let execution = execution.clone();
        std::thread::spawn(move || {
            let cancel = CancelFlag::new();
            client.await_result(&execution, &cancel)
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(500));
    let key = ReconciliationKey::generate();
    let unit = UnitHandle::mint();
    let stopped = client
        .terminate(&unit, std::time::Duration::from_secs(5), &key)
        .expect("terminate must serve during a pending await");
    assert_eq!(stopped.unit_identity, "concurrent01");
    let outcome = waiter
        .join()
        .expect("waiter joins")
        .expect("await completes");
    assert_eq!(outcome, ExecutionOutcome::Exited(0));
}

#[test]
fn swap_after_resolve_executes_pinned_bytes() {
    use std::os::unix::fs::PermissionsExt;
    // Same-FD anchor end to end: resolve pins the verified open
    // description, the path is replaced before spawn, and hosting
    // still executes the pinned original (the swapped bytes never
    // execute without a fresh verify — the re-resolve below
    // refuses).
    let dir = tempfile::tempdir().expect("tempdir");
    let peer = example_binary("example_guest");
    let bytes = std::fs::read(&peer).expect("read peer binary");
    let seat_copy = dir.path().join("seat-copy");
    std::fs::write(&seat_copy, &bytes).expect("stage seat copy");
    std::fs::set_permissions(&seat_copy, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let digest = {
        use sha2::Digest;
        sha2::Sha256::digest(&bytes).into()
    };
    let entry = Guest {
        name: "seat-copy".to_string(),
        role: "example".to_string(),
        protocol: 1,
        source: SourceLocation::Path {
            source_path: seat_copy.clone(),
            digest,
        },
        arguments: Vec::new(),
    };
    let ctx = ResolveCtx::new(dir.path().join("cache"));
    let resolved = resolve(&entry, &ctx).expect("seat copy resolves");
    // Swap the PATH (rename a fresh file over it — a new inode)
    // after resolution: the pinned FD still names the verified
    // original, which is what executes. (An in-place same-inode
    // rewrite would be a trusted-owner write, closed by the
    // ownership gate rather than the FD — the anchor defeats
    // pathname swaps.)
    let swap = dir.path().join("swap");
    std::fs::write(&swap, b"#!/bin/sh\nexit 99\n").expect("stage swap");
    std::fs::set_permissions(&swap, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::rename(&swap, &seat_copy).expect("swap over path");
    let offered = ["example".to_string()];
    let (mut host, negotiated) =
        host_registered(resolved, &[], &offered, short_deadlines()).expect("pinned bytes execute");
    assert_eq!(negotiated, vec!["example".to_string()]);
    host.shutdown().expect("clean shutdown");
    // A fresh resolve admits the swap only by re-verifying — and
    // the swapped bytes fail the registered digest.
    let error = resolve(&entry, &ctx).expect_err("swapped bytes must refuse");
    assert!(
        error.to_string().contains("digest mismatch"),
        "got: {error}"
    );
}

#[test]
fn registered_nonempty_contribution_merges() {
    use cistella::framework::prepare::{ExtensionSpawn, run_extension_prepare};
    // A non-hook registered extension contributes for real: the
    // agentmux vector's env pair plus its read-only bus mount merge
    // through the central gates (the full contribution offer set
    // lets the peer advertise without tripping closed
    // negotiation; the RW-mount refusal, credential admission, and
    // lattice evaluation still own enforcement). The profile
    // carries no acceptances (accepted names are reserved against
    // contributions); the acknowledged policy excuses the
    // contributed names instead.
    let (entry, ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-registered".to_string()],
    );
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let policy_dir = tempfile::tempdir().expect("tempdir");
    let mut text = String::from("format_version = 1\n");
    for name in ["AGENTMUX_BUNDLE", "AGENTMUX_SESSION"] {
        text.push_str("[[acknowledgements]]\nname = \"");
        text.push_str(name);
        text.push_str("\"\n");
    }
    std::fs::write(policy_dir.path().join("policies.toml"), text).expect("write policy");
    let policy = cistella::framework::policy::PolicySet::load(Some(policy_dir.path()))
        .expect("policy loads");
    let plan = run_extension_prepare(
        ExtensionSpawn::Registered(resolved),
        std::path::Path::new("/nonexistent-exe-dir"),
        &[],
        &minimal_profile(),
        &[],
        &policy,
    )
    .expect("nonempty prepare merges");
    assert_eq!(
        plan.merged.environment,
        vec![
            ("AGENTMUX_BUNDLE".to_string(), "seat-7f3a".to_string()),
            ("AGENTMUX_SESSION".to_string(), "session-9c1e".to_string()),
        ]
    );
    assert_eq!(plan.merged.mounts.len(), 1);
    assert_eq!(
        plan.merged.mounts[0].host_source,
        "/run/vector/agentmux-bus"
    );
    assert!(plan.merged.guest_hooks.is_empty());
}
