//! Registered-guest hosting: verified-FD spawn, closed hello, and
//! the pre-planning role compare.
//!
//! The deterministic peer (`fake_guest`) stands in for a registered
//! binary: entries point at it by path with a freshly computed
//! digest, so every test exercises the real resolve-then-host path
//! (no stubs, no unwired seams).

use cistella::framework::contract::Deadlines;
use cistella::framework::guest::host_registered;
use cistella::guest_registry::{GuestSource, RegisteredGuest};
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
    args: Vec<String>,
) -> (RegisteredGuest, ResolveCtx, tempfile::TempDir) {
    use sha2::Digest;
    let path = example_binary(binary);
    let bytes = std::fs::read(&path).expect("read peer binary");
    let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
    let entry = RegisteredGuest {
        name: "test-peer".to_string(),
        role: role.to_string(),
        protocol: 1,
        source: GuestSource::Path {
            path,
            sha256: digest,
        },
        args,
    };
    let dir = tempfile::tempdir().expect("tempdir");
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
