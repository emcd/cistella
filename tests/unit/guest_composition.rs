//! Extension composition tests: N-arity merge, per-plan
//! admission gates, hook cardinality, finalize records-only, and
//! the ≥3-hosted-guest runner proof (split from guest_hosting
//! at the file-size limit). Single-guest prepare stays in
//! guest_hosting.

/// `/tmp` slice snapshot for one fixture session (prefix
/// `cistella-tmp-<session>-`).
fn session_slice_snapshot(session: &str) -> Vec<std::path::PathBuf> {
    let prefix = format!("cistella-tmp-{session}-");
    std::fs::read_dir("/tmp")
        .expect("/tmp readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect()
}

/// Sweep rule (todos/51): a fixture that orphans its session slice
/// proves attribution first — exactly the slices minted since its
/// own snapshot (pre-existing seat litter excluded, concurrent-run
/// interference fails loudly) — then sweeps ONLY that delta.
/// Assert-before-sweep, so a failure never reaps a foreign slice.
fn sweep_minted_slices(before: &[std::path::PathBuf], session: &str) {
    let after = session_slice_snapshot(session);
    let minted: Vec<_> = after.iter().filter(|path| !before.contains(path)).collect();
    assert_eq!(
        minted.len(),
        1,
        "exactly the fixture slice orphans attributed to {session}: {minted:?}"
    );
    for slice in minted {
        std::fs::remove_dir_all(slice).expect("sweep fixture slice");
    }
}

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

fn peer_entry(
    binary: &str,
    role: &str,
    arguments: Vec<String>,
) -> (
    cistella::guest_registry::Guest,
    cistella::guest_source::ResolveCtx,
    tempfile::TempDir,
) {
    use sha2::Digest;
    use std::os::unix::fs::PermissionsExt;
    let source = example_binary(binary);
    let bytes = std::fs::read(&source).expect("read peer binary");
    let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join(binary);
    std::fs::write(&path, &bytes).expect("stage peer copy");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let entry = cistella::guest_registry::Guest {
        name: "test-peer".to_string(),
        role: role.to_string(),
        protocol: 2,
        source: cistella::guest_registry::SourceLocation::Path {
            source_path: path,
            digest,
        },
        arguments,
    };
    let ctx = cistella::guest_source::ResolveCtx::new(dir.path().join("cache"));
    (entry, ctx, dir)
}

fn peer_entry_named(
    binary: &str,
    name: &str,
    role: &str,
    arguments: Vec<String>,
) -> (
    cistella::guest_registry::Guest,
    cistella::guest_source::ResolveCtx,
    tempfile::TempDir,
) {
    // Distinct names: resolution guarantees unique guest
    // identities in production (duplicates refuse), and
    // owner-grouping keys on that identity.
    use sha2::Digest;
    use std::os::unix::fs::PermissionsExt;
    let source = example_binary(binary);
    let bytes = std::fs::read(&source).expect("read peer binary");
    let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join(binary);
    std::fs::write(&path, &bytes).expect("stage peer copy");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let entry = cistella::guest_registry::Guest {
        name: name.to_string(),
        role: role.to_string(),
        protocol: 2,
        source: cistella::guest_registry::SourceLocation::Path {
            source_path: path,
            digest,
        },
        arguments,
    };
    let ctx = cistella::guest_source::ResolveCtx::new(dir.path().join("cache"));
    (entry, ctx, dir)
}

fn minimal_profile() -> cistella::profile::Profile {
    toml::from_str(
        "image = \"localhost/cistella/opencode:example\"\n\
         mounts = []\n",
    )
    .expect("minimal profile parses")
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
        message.contains("registered:test-peer and registered:test-peer"),
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

fn sibling_exe_dir() -> std::path::PathBuf {
    // Real shipped siblings beside the driver (`cargo build`
    // covers them in full runs).
    std::env::current_exe()
        .expect("current test binary")
        .ancestors()
        .nth(2)
        .expect("target/<profile> ancestors")
        .to_path_buf()
}

fn group_dead(pgid: u32) -> bool {
    // Group probe (kill(-pgid, 0)): the recorded process group
    // has no surviving members. Leader-only kill(pid, 0) is
    // NOT used here — it proves leader absence, not group
    // quiescence (a surviving descendant would pass it).
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(-(pgid as i32)), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

fn bad_peer(dir: &tempfile::TempDir) -> cistella::guest_registry::Guest {
    // An executable that sleeps past the host hello write, then
    // dies without answering: hosting fails deterministically at
    // hello-read EOF (an immediate `exit 1` races the hello
    // write into EPIPE flakiness). Fires after healthy guests
    // are already hosted, exercising abort with
    // previously-hosted groups and lease retention.
    use sha2::Digest;
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("dying-guest");
    std::fs::write(&path, b"#!/bin/sh\nsleep 2\nexit 1\n").expect("stage dying guest");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let bytes = std::fs::read(&path).expect("read peer binary");
    let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
    cistella::guest_registry::Guest {
        name: "dying-guest".to_string(),
        role: "test-cap".to_string(),
        protocol: 2,
        source: cistella::guest_registry::SourceLocation::Path {
            source_path: path,
            digest,
        },
        arguments: Vec::new(),
    }
}

#[test]
fn three_guests_compose_through_prepare_many() {
    // N-runner proof (extension-composition 3.2): landlock plus
    // tmpdir (both real shipped guests) plus a fixture peer
    // through the real `prepare_many` — declaration order,
    // per-plan gates, one atomic merge, reverse shutdown.
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (peer, _ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-vector-rw-mount".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, lease) = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Tmpdir,
            ExtensionResolution::Registered(peer),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-triple-1",
        lease_base.path(),
        &[],
    )
    .expect("three guests compose");
    assert_eq!(plan.merged.placements.len(), 2, "slice plus peer bind");
    assert_eq!(
        plan.merged.placements[0].container_target, "/tmp",
        "declaration order: tmpdir first"
    );
    assert_eq!(
        plan.merged.placements[1].container_target, "/vectmp",
        "declaration order: peer second"
    );
    assert_eq!(plan.merged.guest_hooks.len(), 1, "landlock hook stages");
    assert_eq!(plan.merged.cleanup.len(), 1, "slice declared");
    assert!(lease.is_some());
    // The tmpdir slice is fixture litter (reap is proven
    // separately — never leave /tmp dirty on success paths).
    for path in &plan.merged.cleanup {
        std::fs::remove_dir_all(path).expect("remove fixture slice");
    }
}

#[test]
fn later_guest_gating_refuses_with_clean_shutdown() {
    // Per-plan gating covers the LAST guest too: its
    // unadvertised hooks refuse at its own gate, and the
    // already-hosted groups shut down cleanly (the surfaced
    // error is the gate refusal, never shutdown residue).
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (peer, _ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-hook-unadvertised".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let slice_before = session_slice_snapshot("test-session-late-gate");
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Tmpdir,
            ExtensionResolution::Registered(peer),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-late-gate",
        lease_base.path(),
        &[],
    )
    .expect_err("later guest gate must refuse");
    assert!(
        error
            .to_string()
            .contains("unadvertised contribution type: guest-hooks"),
        "gate refusal, got: {error}"
    );
    // The clean shutdown drops the lease but orphans the minted
    // slice (session-attributed, like the collide-slice case):
    // prove attribution, then sweep — no run-over-run litter.
    sweep_minted_slices(&slice_before, "test-session-late-gate");
}

#[test]
fn mid_host_failure_retains_lease() {
    // A guest that dies during hosting, after a healthy guest
    // is hosted: the hosted group shuts down, the error names
    // the hosting failure, and the lease is retained (shutdown
    // of the failed host is unprovable post-hoc).
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::lock::PrepareLease;
    use cistella::profile::ExtensionResolution;
    let stage = tempfile::tempdir().expect("tmpdir");
    let peer = bad_peer(&stage);
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let session = "test-session-host-fail";
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Registered(peer),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        session,
        lease_base.path(),
        &[],
    )
    .expect_err("hosting failure must refuse");
    assert!(
        error.to_string().contains("dying-guest")
            || error.to_string().contains("eof at frame boundary"),
        "hosting failure surfaces, got: {error}"
    );
    let path = PrepareLease::path_for(lease_base.path(), session).expect("lease path");
    assert!(
        path.exists(),
        "unprovable failed-host shutdown retains the lease"
    );
}

#[test]
fn mid_prepare_failure_preserves_original_error() {
    // A guest failing INSIDE prepare after healthy guests
    // transacted: clean shutdowns preserve the incoming error
    // (quiescence proven, lease drops) — residue never masks it.
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::lock::PrepareLease;
    use cistella::profile::ExtensionResolution;
    let (peer, _ctx, _dir) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-credential-shape-fault".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let session = "test-session-prepare-fail";
    let slice_before = session_slice_snapshot(session);
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Tmpdir,
            ExtensionResolution::Registered(peer),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        session,
        lease_base.path(),
        &[],
    )
    .expect_err("prepare fault must refuse");
    assert!(
        error.to_string().contains("shape violation"),
        "original fault preserved, got: {error}"
    );
    let path = PrepareLease::path_for(lease_base.path(), session).expect("lease path");
    assert!(
        !path.exists(),
        "clean shutdowns prove quiescence: lease drops"
    );
    // The dropped lease still orphans the minted slice: prove
    // attribution, then sweep — no run-over-run litter.
    sweep_minted_slices(&slice_before, session);
}

#[test]
fn collision_refusal_drops_lease_without_attribution() {
    // Two peers contributing the same name: the merge refuses
    // naming both, clean shutdowns drop the lease, and no
    // cleanup is recorded (nothing attributable exists).
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::lock::PrepareLease;
    use cistella::profile::ExtensionResolution;
    let (first, _ctx1, _dir1) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-echo-session-id".to_string()],
    );
    let (second, _ctx2, _dir2) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-echo-session-id".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let session = "test-session-collide";
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Registered(first),
            ExtensionResolution::Registered(second),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        session,
        lease_base.path(),
        &[],
    )
    .expect_err("collision must refuse");
    assert!(
        error.to_string().contains("SESSION_ID_ECHO"),
        "collision names the name, got: {error}"
    );
    let path = PrepareLease::path_for(lease_base.path(), session).expect("lease path");
    assert!(!path.exists(), "lease drops on clean collision abort");
}

#[test]
fn duplicate_extension_entries_refuse_at_resolution() {
    // Same extension declared twice: profile resolution
    // refuses naming the declaration (a guest is never hosted
    // twice).
    use cistella::guest_registry::Registry;
    let registry = Registry::parse(
        "format-version = 1\n\
         [[extensions]]\n\
         name = \"file-extension\"\n\
         role = \"test-cap\"\n\
         protocol = 2\n\
         source-digest = { algorithm = 'sha256', value = \"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\" }\n\
         source-path = \"/opt/guests/ext\"\n",
    )
    .expect("registry parses");
    let prof: cistella::profile::Profile = toml::from_str(
        "image = \"localhost/cistella/opencode:example\"\n\
         mounts = []\n\
         [[extensions]]\n\
         name = \"file-extension\"\n\
         [[extensions]]\n\
         name = \"file-extension\"\n",
    )
    .expect("profile parses");
    let error = prof
        .resolve_extensions(&registry)
        .expect_err("duplicate must refuse");
    assert!(
        error.to_string().contains("duplicate extension"),
        "got: {error}"
    );
}

#[test]
fn placement_less_plan_conducts_topology() {
    // Permissions-only shape: an empty placement plan with no
    // hooks flows through topology preparation demanding
    // nothing (a placement-less isolator is supported by the
    // empty plan, never accept-then-ignore).
    use cistella::framework::contract::EffectivePermissions;
    use cistella::framework::hooks::prepare_conduct_topology;
    let effective = EffectivePermissions::assemble(&[], &[], &[], &[]).expect("empty assembles");
    let (revised, placements, _) =
        prepare_conduct_topology(Vec::new(), &[], &[], &effective, &[], "/work")
            .expect("empty plan conducts");
    assert!(revised.is_empty() && placements.is_empty());
}

#[test]
fn broad_claim_constrains_only_its_owner() {
    // Guest A claims `.*` over its own acknowledged name while
    // guest B contributes an unacknowledged name: owner-grouped
    // evaluation upholds A's claim for A's names and leaves B
    // alone (global flattening would refuse B).
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::profile::ExtensionResolution;
    let (first, _ctx1, _dir1) = peer_entry_named(
        "fake_guest",
        "broad-guest",
        "test-cap",
        vec!["--mode=prepare-claim-broad".to_string()],
    );
    let (second, _ctx2, _dir2) = peer_entry_named(
        "fake_guest",
        "echo-guest",
        "test-cap",
        vec!["--mode=prepare-echo-session-id".to_string()],
    );
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut text = String::from("format_version = 1\n");
    text.push_str("[[acknowledgements]]\nname = \"MY_NAME\"\n");
    std::fs::write(dir.path().join("policies.toml"), text).expect("write policies.toml");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(dir.path())).expect("policy loads");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let (plan, _lease) = prepare_selected_extension(
        vec![
            ExtensionResolution::Registered(first),
            ExtensionResolution::Registered(second),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        "test-session-broad-claim",
        lease_base.path(),
        &[],
    )
    .expect("broad claim constrains only its owner");
    assert_eq!(plan.merged.environment.len(), 2);
}

#[test]
fn mid_host_failure_across_three_groups_retains_ordered_lease() {
    // N=3 abort evidence: two healthy guests hosted in order,
    // third dies during hosting. The retained lease records
    // BOTH hosted groups in hosting order (PID lines) plus no
    // pending marker ambiguity — the failure names the third
    // guest, never residue from the clean shutdowns.
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::lock::PrepareLease;
    use cistella::profile::ExtensionResolution;
    let (peer, _pctx, _pdir) = peer_entry("example_guest", "example", Vec::new());
    let stage = tempfile::tempdir().expect("tmpdir");
    let dying = bad_peer(&stage);
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let session = "test-session-host-fail-3";
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Landlock,
            ExtensionResolution::Registered(peer),
            ExtensionResolution::Registered(dying),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        session,
        lease_base.path(),
        &[],
    )
    .expect_err("third-guest hosting failure must refuse");
    assert!(
        error.to_string().contains("eof at frame boundary"),
        "hosting failure surfaces, got: {error}"
    );
    let path = PrepareLease::path_for(lease_base.path(), session).expect("lease path");
    let content = std::fs::read_to_string(&path).expect("retained lease readable");
    let guests: Vec<&str> = content
        .lines()
        .filter(|line| line.starts_with("guest "))
        .collect();
    assert_eq!(
        guests.len(),
        2,
        "both hosted groups recorded in order, third never hosted: {content:?}"
    );
    assert_ne!(
        guests[0], guests[1],
        "distinct hosted PIDs in hosting order: {content:?}"
    );
    // No stranded groups: every recorded hosted GROUP is dead
    // after the abort (reverse shutdown reaped all members —
    // residue would be a live group here). Lease `guest PID
    // PGID` lines give both; the group probe is the load-bearing
    // one (leader-only probing cannot see descendants).
    for line in &guests {
        let mut fields = line.split_whitespace();
        let pid: u32 = fields
            .nth(1)
            .expect("guest line carries a pid")
            .parse()
            .expect("pid parses");
        let pgid: u32 = fields
            .next()
            .expect("guest line carries a pgid")
            .parse()
            .expect("pgid parses");
        assert!(
            group_dead(pgid),
            "hosted group {pgid} (leader {pid}) reaped by abort, still live"
        );
    }
}

#[test]
fn collision_with_minted_slice_orphans_for_gc() {
    // Merge refusal after a guest minted host state: the slice
    // EXISTS (attributed by session id for gc), the lease drops
    // (clean shutdowns prove quiescence), and nothing is reaped
    // without admission — unadmitted declarations are never
    // deletion authority, so gc owns the orphan by design. The
    // gc mechanism itself (attributed orphan reaps, pending
    // lease skips) is pinned in cleanup.rs; this test proves
    // the attribution half of that chain.
    use cistella::framework::prepare_select::prepare_selected_extension;
    use cistella::lock::PrepareLease;
    use cistella::profile::ExtensionResolution;
    let (first, _ctx1, _dir1) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-echo-session-id".to_string()],
    );
    let (second, _ctx2, _dir2) = peer_entry(
        "fake_guest",
        "test-cap",
        vec!["--mode=prepare-echo-session-id".to_string()],
    );
    let policy_dir = tempfile::tempdir().expect("tmpdir");
    let policy =
        cistella::framework::policy::PolicySet::load(Some(policy_dir.path())).expect("defaults");
    let lease_base = tempfile::tempdir().expect("tmpdir");
    let session = "test-session-collide-slice";
    let error = prepare_selected_extension(
        vec![
            ExtensionResolution::Tmpdir,
            ExtensionResolution::Registered(first),
            ExtensionResolution::Registered(second),
        ],
        &sibling_exe_dir(),
        &minimal_profile(),
        &[],
        &policy,
        session,
        lease_base.path(),
        &[],
    )
    .expect_err("collision must refuse");
    assert!(
        error.to_string().contains("SESSION_ID_ECHO"),
        "got: {error}"
    );
    let path = PrepareLease::path_for(lease_base.path(), session).expect("lease path");
    assert!(!path.exists(), "lease drops on clean collision abort");
    // The minted slice orphans with session attribution for gc;
    // the test sweeps its own litter after proving existence.
    let prefix = format!("cistella-tmp-{session}-");
    let orphans: Vec<std::path::PathBuf> = std::fs::read_dir("/tmp")
        .expect("/tmp readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect();
    assert_eq!(
        orphans.len(),
        1,
        "exactly the minted slice orphans attributed: {orphans:?}"
    );
    std::fs::remove_dir_all(&orphans[0]).expect("sweep fixture slice");
}
