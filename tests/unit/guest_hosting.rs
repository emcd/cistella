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
/// Shipped sibling binaries (not in `examples/`) resolve by
/// fallback to the profile dir (e.g. `cistella-extension-tmpdir`).
fn example_binary(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let profile_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors");
    let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for dir in [profile_dir.join("examples"), profile_dir.to_path_buf()] {
        if let Ok(entries) = std::fs::read_dir(&dir) {
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
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .unwrap_or_else(|| {
            panic!(
                "example binary {name} not found in {}; run `cargo build` first",
                profile_dir.display()
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
    let dir = tempfile::tempdir().expect("tmpdir");
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
        protocol: 2,
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
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, _lease) = run_extension_prepare(
        ExtensionSpawn::Registered(resolved),
        std::path::Path::new("/nonexistent-exe-dir"),
        &[],
        &minimal_profile(),
        &[],
        &policy,
        "test-session-7f3a",
        lease_base.path(),
    )
    .expect("registered prepare converges");
    assert!(plan.merged.environment.is_empty());
    assert!(plan.merged.placements.is_empty());
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
    let rendezvous = tempfile::tempdir().expect("tmpdir");
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
    let dir = tempfile::tempdir().expect("tmpdir");
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
        protocol: 2,
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
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let mut text = String::from("format_version = 1\n");
    for name in ["AGENTMUX_BUNDLE", "AGENTMUX_SESSION"] {
        text.push_str("[[acknowledgements]]\nname = \"");
        text.push_str(name);
        text.push_str("\"\n");
    }
    std::fs::write(policy_dir.path().join("policies.toml"), text).expect("write policy");
    let policy = cistella::framework::policy::PolicySet::load(Some(policy_dir.path()))
        .expect("policy loads");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, _lease) = run_extension_prepare(
        ExtensionSpawn::Registered(resolved),
        std::path::Path::new("/nonexistent-exe-dir"),
        &[],
        &minimal_profile(),
        &[],
        &policy,
        "test-session-7f3a",
        lease_base.path(),
    )
    .expect("nonempty prepare merges");
    assert_eq!(
        plan.merged.environment,
        vec![
            ("AGENTMUX_BUNDLE".to_string(), "seat-7f3a".to_string()),
            ("AGENTMUX_SESSION".to_string(), "session-9c1e".to_string()),
        ]
    );
    assert_eq!(plan.merged.placements.len(), 1);
    assert_eq!(
        plan.merged.placements[0].host_source,
        "/run/vector/agentmux-bus"
    );
    assert!(plan.merged.guest_hooks.is_empty());
}

#[test]
fn session_id_threads_into_prepare() {
    use cistella::framework::prepare::{ExtensionSpawn, run_extension_prepare};
    // The runner's session id reaches the guest verbatim: the peer
    // echoes the request's session_id back as an env contribution.
    // Old-shape peers (generic request parsing, predating the
    // field) complete against session-id-carrying requests —
    // decoder tolerance pinned against actual peer behavior.
    let (entry, ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-echo-session-id".to_string()],
    );
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let text = "format_version = 1\n[[acknowledgements]]\nname = \"SESSION_ID_ECHO\"\n";
    std::fs::write(policy_dir.path().join("policies.toml"), text).expect("write policy");
    let policy = cistella::framework::policy::PolicySet::load(Some(policy_dir.path()))
        .expect("policy loads");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, _lease) = run_extension_prepare(
        ExtensionSpawn::Registered(resolved),
        std::path::Path::new("/nonexistent-exe-dir"),
        &[],
        &minimal_profile(),
        &[],
        &policy,
        "test-session-echo-1",
        lease_base.path(),
    )
    .expect("session id threads through");
    assert_eq!(
        plan.merged.environment,
        vec![(
            "SESSION_ID_ECHO".to_string(),
            "test-session-echo-1".to_string()
        )]
    );
}

#[test]
fn extension_rw_mount_contribution_merges() {
    use cistella::framework::prepare::{ExtensionSpawn, run_extension_prepare};
    // No blanket refusal: a valid-shape RW triple merges through
    // the central validators like any declared triple (reserved
    // collisions still refuse at the contract layer).
    let (entry, ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-rw-mount".to_string()],
    );
    let resolved = resolve(&entry, &ctx).expect("peer resolves");
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, _lease) = run_extension_prepare(
        ExtensionSpawn::Registered(resolved),
        std::path::Path::new("/nonexistent-exe-dir"),
        &[],
        &minimal_profile(),
        &[],
        &policy,
        "test-session-rw-1",
        lease_base.path(),
    )
    .expect("RW contribution merges");
    assert_eq!(plan.merged.placements.len(), 1);
    let placement = &plan.merged.placements[0];
    assert_eq!(placement.host_source, "/tmp");
    assert_eq!(placement.container_target, "/vectmp");
    assert_eq!(
        plan.merged.permissions,
        vec![cistella::framework::contract::EffectiveGrant {
            target: "/vectmp".to_string(),
            rights: cistella::framework::contract::Rights::Full,
            claimants: vec![(
                cistella::framework::contract::Provenance::Extension("test-peer".to_string()),
                cistella::framework::contract::Rights::Full,
            )],
            binding_intent_ro: false,
        }],
        "rw placement joins Full with test provenance"
    );
}

/// Stages the deterministic peer as the shipped landlock binary
/// (owned 0755 copy under its sibling-relative name — same
/// umask-independent discipline as registered staging).
fn stage_landlock_peer(dir: &tempfile::TempDir) {
    use std::os::unix::fs::PermissionsExt;
    let bytes = std::fs::read(example_binary("fake_guest")).expect("read peer binary");
    let path = dir.path().join("cistella-extension-landlock");
    std::fs::write(&path, &bytes).expect("stage landlock peer");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn landlock_plus_registered_compose_through_production_select() {
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    // The v1 pair through the production selection entry point
    // (not a hand-composed fixture): landlock contributes its
    // hook, the registered peer its env pair plus RO mount, one
    // atomic central merge carries both.
    let exe = tempfile::tempdir().expect("tmpdir");
    stage_landlock_peer(&exe);
    let (entry, _ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-registered".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let mut text = String::from("format_version = 1\n");
    for name in ["AGENTMUX_BUNDLE", "AGENTMUX_SESSION"] {
        text.push_str("[[acknowledgements]]\nname = \"");
        text.push_str(name);
        text.push_str("\"\n");
    }
    std::fs::write(policy_dir.path().join("policies.toml"), text).expect("write policy");
    let policy = cistella::framework::policy::PolicySet::load(Some(policy_dir.path()))
        .expect("policy loads");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, _lease) = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Registered(entry),
        ],
        exe.path(),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-pair-1",
        lease_base.path(),
        &[
            vec!["--mode=extension-landlock-hook".to_string()],
            Vec::new(),
        ],
    )
    .expect("pair composes");
    assert_eq!(
        plan.merged.environment,
        vec![
            ("AGENTMUX_BUNDLE".to_string(), "seat-7f3a".to_string()),
            ("AGENTMUX_SESSION".to_string(), "session-9c1e".to_string()),
        ]
    );
    assert_eq!(plan.merged.placements.len(), 1);
    assert_eq!(
        plan.merged.placements[0].host_source,
        "/run/vector/agentmux-bus"
    );
    assert_eq!(plan.merged.guest_hooks.len(), 1);
}

#[test]
fn two_non_landlock_extensions_compose() {
    // N-law: arity is not a refusal reason. Two
    // empty-contribution registered peers host in declaration
    // order, transact, shut down in reverse, and merge empty.
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (first, _ctx1, _dir1) = peer_entry("example_guest", "example", Vec::new());
    let (second, _ctx2, _dir2) = peer_entry("example_guest", "example", Vec::new());
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, lease) = prepare_selected_extension(
        vec![
            ExtensionResolution::Registered(first),
            ExtensionResolution::Registered(second),
        ],
        std::path::Path::new("/nonexistent-exe-dir"),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-pair-compose",
        lease_base.path(),
        &[],
    )
    .expect("two non-landlock extensions compose");
    assert!(plan.merged.environment.is_empty());
    assert!(plan.merged.placements.is_empty());
    assert!(plan.merged.permissions.is_empty());
    assert!(plan.merged.guest_hooks.is_empty());
    assert!(lease.is_some(), "composed prepare holds a lease");
}

#[test]
fn tmpdir_extension_mints_contributes_declares() {
    use cistella::framework::prepare::{ExtensionSpawn, run_extension_prepare};
    // The shipped tmpdir extension through the shipped path:
    // mints an owner-only slice, contributes the RW triple
    // targeting /tmp, and declares it for cleanup — all three
    // verified, not just trusted. No registry involved (shipped
    // resolution); the sibling binary must be built (`cargo
    // build` covers it in full runs).
    let exe_dir = std::env::current_exe()
        .expect("current test binary")
        .ancestors()
        .nth(2)
        .expect("target/<profile> ancestors")
        .to_path_buf();
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, _lease) = run_extension_prepare(
        ExtensionSpawn::Tmpdir,
        &exe_dir,
        &[],
        &minimal_profile(),
        &[],
        &policy,
        "test-session-tmp-1",
        lease_base.path(),
    )
    .expect("tmpdir prepare merges");
    assert_eq!(plan.merged.placements.len(), 1);
    let placement = &plan.merged.placements[0];
    assert_eq!(placement.container_target, "/tmp");
    assert_eq!(plan.merged.cleanup.len(), 1);
    assert_eq!(plan.merged.cleanup[0], placement.host_source);
    // Hardcoded host-/tmp parent (no environment selection):
    // fast tests mint real host-/tmp slices (session-unique,
    // gc-reaped — same discipline live tests accept).
    let prefix = "/tmp/cistella-tmp-test-session-tmp-1-";
    assert!(
        placement.host_source.starts_with(prefix),
        "slice attributed to the session under host /tmp, got: {}",
        placement.host_source
    );
    let metadata = std::fs::symlink_metadata(&placement.host_source).expect("slice exists");
    assert!(metadata.is_dir(), "slice is a directory");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o700,
        "slice is owner-only"
    );
    assert!(plan.merged.guest_hooks.is_empty());
    // The test owns fixture litter (reap is 3.2's job, proven
    // separately — never leave /tmp dirty on success paths).
    std::fs::remove_dir(&placement.host_source).expect("remove fixture slice");
}

#[test]
fn unadvertised_hook_does_not_launder_through_pair() {
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    // Capability laundering probe: the registered peer contributes
    // a hook WITHOUT advertising guest-hooks, while landlock (its
    // pair partner) advertises the type. The per-plan gate must
    // refuse the whole transaction — the union gate alone would
    // admit it through landlock's advertisement.
    let exe = tempfile::tempdir().expect("tmpdir");
    stage_landlock_peer(&exe);
    let (entry, _ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-hook-unadvertised".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Registered(entry),
        ],
        exe.path(),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-launder-1",
        lease_base.path(),
        &[
            vec!["--mode=extension-landlock-hook".to_string()],
            Vec::new(),
        ],
    )
    .expect_err("unadvertised hook must refuse whole");
    assert!(
        error.to_string().contains("unadvertised contribution type"),
        "got: {error}"
    );
}

#[test]
fn second_guest_host_failure_shuts_first_and_retains() {
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    // Mid-pair hosting failure: the first guest shuts down too
    // (no stranded peer — the error is the contract refusal, not
    // residue), and the lease is retained (the failed host's
    // shutdown is unprovable post-hoc — stale-gc heals).
    let exe = tempfile::tempdir().expect("tmpdir");
    stage_landlock_peer(&exe);
    let (entry, _ctx, _dir) = peer_entry(
        "fake_guest",
        "other-role",
        vec!["--mode=hello-real-capabilities".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Registered(entry),
        ],
        exe.path(),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-abort-1",
        lease_base.path(),
        &[
            vec!["--mode=extension-landlock-hook".to_string()],
            Vec::new(),
        ],
    )
    .expect_err("role mismatch must refuse");
    assert!(error.to_string().contains("role mismatch"), "got: {error}");
    // SAFETY: getuid is inherently safe.
    let seat = unsafe { nix::libc::getuid() };
    assert!(
        lease_base
            .path()
            .join(format!("cistella-state-{seat}"))
            .join("cistella-prepare-test-session-abort-1")
            .exists(),
        "uncertainty retained for stale-gc"
    );
}

#[test]
fn unadvertised_credentials_do_not_launder_through_sibling() {
    // Sibling advertises credentials; the emitter does not.
    // The emitter's own per-plan gate must refuse — the union
    // gate must never see the laundered handles.
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (emitter, _ctx1, _dir1) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-ssh-almost".to_string()],
    );
    let (sibling, _ctx2, _dir2) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-ssh-advertised".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Registered(emitter),
            ExtensionResolution::Registered(sibling),
        ],
        std::path::Path::new("/nonexistent-exe-dir"),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-creds-launder",
        lease_base.path(),
        &[],
    )
    .expect_err("unadvertised credentials must refuse");
    assert!(
        error.to_string().contains("credentials"),
        "credential gate, got: {error}"
    );
}

#[test]
fn unadvertised_permissions_do_not_launder_through_sibling() {
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (emitter, _ctx1, _dir1) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-perms-unadvertised".to_string()],
    );
    let (sibling, _ctx2, _dir2) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-rw-mount".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Registered(emitter),
            ExtensionResolution::Registered(sibling),
        ],
        std::path::Path::new("/nonexistent-exe-dir"),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-perms-launder",
        lease_base.path(),
        &[],
    )
    .expect_err("unadvertised permissions must refuse");
    assert!(
        error.to_string().contains("permissions"),
        "permission gate, got: {error}"
    );
}

#[test]
fn foreign_claim_refuses_before_concat() {
    // Claimant contributes no names and reaches only the
    // sibling's name: guest-local scope refuses at its own
    // gate (first in declaration order), before any concat.
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (claimant, _ctx1, _dir1) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-claim-foreign".to_string()],
    );
    let (sibling, _ctx2, _dir2) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-agentmux".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Registered(claimant),
            ExtensionResolution::Registered(sibling),
        ],
        std::path::Path::new("/nonexistent-exe-dir"),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-claim-foreign",
        lease_base.path(),
        &[],
    )
    .expect_err("foreign claim must refuse");
    assert!(
        error.to_string().contains("beyond its transaction"),
        "claim scope, got: {error}"
    );
}

#[test]
fn single_guest_returning_two_hooks_refuses() {
    // One guest, two valid hook requests: admission refuses the
    // second request pre-merge (counting nonempty plans alone
    // would admit it).
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (guest, _ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-double-hook".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let error = prepare_selected_extension(
        vec![ExtensionResolution::Registered(guest)],
        std::path::Path::new("/nonexistent-exe-dir"),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-double-hook",
        lease_base.path(),
        &[],
    )
    .expect_err("two hook requests from one guest must refuse");
    assert!(
        error.to_string().contains("at most one"),
        "single-set-single-request, got: {error}"
    );
}

#[test]
fn two_single_hook_guests_refuse_naming_both() {
    // Two guests each returning one valid hook: admission names
    // both claimants pre-merge (the wrapper chain is the only
    // true ceiling).
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (first, _ctx1, _dir1) = peer_entry(
        "fake_guest",
        "landlock",
        vec!["--mode=extension-landlock-hook".to_string()],
    );
    let (second, _ctx2, _dir2) = peer_entry(
        "fake_guest",
        "landlock",
        vec!["--mode=extension-landlock-hook".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Registered(first),
            ExtensionResolution::Registered(second),
        ],
        std::path::Path::new("/nonexistent-exe-dir"),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-two-hooks",
        lease_base.path(),
        &[],
    )
    .expect_err("two hook claimants must refuse");
    let message = error.to_string();
    assert!(
        message.contains("more than one guest-hook claimant"),
        "got: {message}"
    );
    assert!(
        message.contains("test-peer and test-peer"),
        "both claimant positions named, got: {message}"
    );
}

#[test]
fn finalize_observations_create_nothing() {
    // Records-only: no observers exist, so the phase returns no
    // notes, touches nothing, and leaves the merged plan whole.
    // Idempotent by construction (pure over the plan).
    use cistella::framework::contract::MergedPlan;
    use cistella::framework::hooks::run_finalize_observations;
    let merged = MergedPlan {
        environment: vec![("A".to_string(), "1".to_string())],
        placements: Vec::new(),
        permissions: Vec::new(),
        policy_claims: Vec::new(),
        guest_hooks: Vec::new(),
        cleanup: vec!["/tmp/cistella-tmp-x".to_string()],
    };
    let first = run_finalize_observations(&merged);
    let second = run_finalize_observations(&merged);
    assert!(first.is_empty() && second.is_empty());
}
