//! Policy lattice + prepare transaction unit tests.
//!
//! File-contract refusal, the four lattice cells, grandfathering,
//! claim partition, and `run_prepare` over a scripted exchange. All
//! diagnostics assertions pin value-freedom (names only).

use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use cistella::framework::contract::{GuestHookRequest, MergeContext, Provenance, Scope, Severity};
use cistella::framework::policy::{PolicySet, acceptance_set, evaluate_all};
use cistella::framework::prepare::{
    EXTENSION_BIN, check_hook_executable, parse_capability, refuse_pending_hook_delivery,
    run_landlock_prepare, run_prepare,
};
use cistella::framework::protocol::{
    Envelope, Exchange, PRE_NEGOTIATION_MAX_FRAME, PROTOCOL_MAJOR, envelope_bytes, parse_envelope,
    read_frame, write_frame,
};
use cistella::profile::{CredentialSurface, Profile};

const FAST: Duration = Duration::from_secs(3);

fn profile() -> Provenance {
    Provenance::Profile
}

fn extension() -> Provenance {
    Provenance::Extension("probe".to_string())
}

fn empty_acceptances() -> HashSet<String> {
    HashSet::new()
}

/// Compiled defaults via load: an absent file means defaults only.
fn defaults() -> PolicySet {
    let dir = tempfile::tempdir().expect("tempdir");
    PolicySet::load(Some(dir.path())).expect("absent file means defaults")
}

#[test]
fn absent_file_means_compiled_defaults() {
    let policy = defaults();
    // Token-shaped names refuse for any provenance, even profile.
    let error = policy
        .evaluate("GITHUB_TOKEN", &profile(), &empty_acceptances(), &[])
        .unwrap_err();
    assert!(error.to_string().contains("GITHUB_TOKEN"));
    assert!(!error.to_string().contains("ghp_"));
    // Case-insensitive like the 0.1.x guard.
    policy
        .evaluate("gh_token", &profile(), &empty_acceptances(), &[])
        .unwrap_err();
    // Ordinary names pass.
    policy
        .evaluate("TERM", &profile(), &empty_acceptances(), &[])
        .unwrap();
}

#[test]
fn file_contract_refusals() {
    // Malformed TOML.
    PolicySet::parse(b"[[denials]").unwrap_err();
    // Unknown version.
    PolicySet::parse(b"format_version = 2\n").unwrap_err();
    // Unknown top-level field.
    PolicySet::parse(b"format_version = 1\nsmuggled = true\n").unwrap_err();
    // Unknown denial field.
    PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = 'X'\nseverity = 'suppressible'\nscope = 'universal'\nextra = 1\n",
    )
    .unwrap_err();
    // Bad regex.
    PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = '([A'\nseverity = 'suppressible'\nscope = 'universal'\n",
    )
    .unwrap_err();
    // Duplicate denial.
    PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = 'X'\nseverity = 'suppressible'\nscope = 'universal'\n[[denials]]\npattern = 'X'\nseverity = 'suppressible'\nscope = 'universal'\n",
    )
    .unwrap_err();
    // Duplicate acknowledgement.
    PolicySet::parse(
        b"format_version = 1\n[[acknowledgements]]\nname = 'X'\n[[acknowledgements]]\nname = 'X'\n",
    )
    .unwrap_err();
    // Bad acknowledgement name (env grammar).
    PolicySet::parse(b"format_version = 1\n[[acknowledgements]]\nname = 'lower'\n").unwrap_err();
    // User inviolable universal impersonates site authority.
    PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = 'X'\nseverity = 'inviolable'\nscope = 'universal'\n",
    )
    .unwrap_err();
}

#[test]
fn user_rule_may_not_weaken_default() {
    // Same pattern string, narrower scope: universal -> on-extensions.
    let bytes = b"format_version = 1\n[[denials]]\npattern = '(?i)^(GITHUB_TOKEN|GH_TOKEN|GITHUB_PAT)$'\nseverity = 'suppressible'\nscope = 'on-extensions'\n";
    let error = PolicySet::parse(bytes).unwrap_err();
    assert!(error.to_string().contains("weakens"));
}

#[test]
fn suppressible_universal_cell_with_acknowledgement() {
    let bytes = b"format_version = 1\n[[denials]]\npattern = '^PROBE_'\nseverity = 'suppressible'\nscope = 'universal'\n";
    let policy = PolicySet::parse(bytes).unwrap();
    policy
        .evaluate("PROBE_VAR", &profile(), &empty_acceptances(), &[])
        .unwrap_err();
    let acked = PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = '^PROBE_'\nseverity = 'suppressible'\nscope = 'universal'\n[[acknowledgements]]\nname = 'PROBE_VAR'\n",
    )
    .unwrap();
    acked
        .evaluate("PROBE_VAR", &profile(), &empty_acceptances(), &[])
        .unwrap();
}

#[test]
fn suppressible_on_extensions_cell_scopes_provenance() {
    let bytes = b"format_version = 1\n[[denials]]\npattern = '^PROBE_'\nseverity = 'suppressible'\nscope = 'on-extensions'\n";
    let policy = PolicySet::parse(bytes).unwrap();
    // Profile value with the same name is unaffected.
    policy
        .evaluate("PROBE_VAR", &profile(), &empty_acceptances(), &[])
        .unwrap();
    // Extension contribution refuses unless acknowledged.
    policy
        .evaluate("PROBE_VAR", &extension(), &empty_acceptances(), &[])
        .unwrap_err();
    let acked = PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = '^PROBE_'\nseverity = 'suppressible'\nscope = 'on-extensions'\n[[acknowledgements]]\nname = 'PROBE_VAR'\n",
    )
    .unwrap();
    acked
        .evaluate("PROBE_VAR", &extension(), &empty_acceptances(), &[])
        .unwrap();
}

#[test]
fn inviolable_on_extensions_refuses_despite_acknowledgement() {
    let bytes = b"format_version = 1\n[[denials]]\npattern = '^PROBE_'\nseverity = 'inviolable'\nscope = 'on-extensions'\n[[acknowledgements]]\nname = 'PROBE_VAR'\n";
    let policy = PolicySet::parse(bytes).unwrap();
    let error = policy
        .evaluate("PROBE_VAR", &extension(), &empty_acceptances(), &[])
        .unwrap_err();
    assert!(error.to_string().contains("Inviolable"));
    // Profile values never match this cell.
    policy
        .evaluate("PROBE_VAR", &profile(), &empty_acceptances(), &[])
        .unwrap();
}

#[test]
fn present_empty_file_refuses_instead_of_defaulting() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("policies.toml"), b"").expect("write empty");
    let error = PolicySet::load(Some(dir.path())).unwrap_err();
    assert!(error.to_string().contains("empty"));
    // And the pure half agrees.
    PolicySet::parse(b"").unwrap_err();
}

#[test]
fn extension_spoof_of_accepted_name_refuses() {
    let policy = defaults();
    let acceptances = acceptance_set(&["GITHUB_TOKEN".to_string()]);
    // Profile value grandfathers; extension value under the same
    // accepted name is spoofing and refuses.
    policy
        .evaluate("GITHUB_TOKEN", &profile(), &acceptances, &[])
        .unwrap();
    let error = policy
        .evaluate("GITHUB_TOKEN", &extension(), &acceptances, &[])
        .unwrap_err();
    assert!(error.to_string().contains("GITHUB_TOKEN"));
}

#[test]
fn shipped_acceptances_grandfathered_against_defaults_only() {
    let policy = defaults();
    let acceptances = acceptance_set(&["GITHUB_TOKEN".to_string()]);
    // Grandfathered: no acknowledgement needed against the default.
    policy
        .evaluate("GITHUB_TOKEN", &profile(), &acceptances, &[])
        .unwrap();
    // But a user suppressible denial still requires acknowledgement.
    let user = PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = '^GITHUB_'\nseverity = 'suppressible'\nscope = 'universal'\n",
    )
    .unwrap();
    user.evaluate("GITHUB_TOKEN", &profile(), &acceptances, &[])
        .unwrap_err();
}

#[test]
fn diagnostics_are_value_free() {
    let policy = defaults();
    let error = policy
        .evaluate("GITHUB_TOKEN", &extension(), &empty_acceptances(), &[])
        .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("GITHUB_TOKEN"));
    assert!(text.contains("Universal"));
    assert!(text.contains("Suppressible"));
}

#[test]
fn evaluate_all_fails_first() {
    let policy = defaults();
    evaluate_all(
        &policy,
        &[
            ("TERM".to_string(), profile()),
            ("GITHUB_TOKEN".to_string(), profile()),
        ],
        &empty_acceptances(),
        &[],
    )
    .unwrap_err();
}

/// Scripted guest: reads one frame, writes one canned response.
fn scripted_peer(
    payload: serde_json::Value,
) -> (
    Exchange<UnixStream, UnixStream>,
    std::thread::JoinHandle<()>,
) {
    let (a, b) = UnixStream::pair().unwrap();
    let host = Exchange::new(a.try_clone().unwrap(), a);
    let handle = std::thread::spawn(move || {
        let mut peer = b;
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        let response = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: envelope.id.clone(),
            op: envelope.op.clone(),
            payload,
        };
        write_frame(
            &mut peer,
            &envelope_bytes(&response),
            PRE_NEGOTIATION_MAX_FRAME,
        )
        .unwrap();
    });
    (host, handle)
}

fn full_caps() -> Vec<String> {
    ["environment", "mounts", "policy-claims", "guest-hooks"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn prepare_merges_valid_response() {
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "PROBE_A", "value": "1"}],
        "mounts": [],
        "policy_claims": [],
        "guest_hooks": [],
    }));
    let policy = defaults();
    let plan = run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        &MergeContext::empty("/home/cistella"),
        &policy,
        &empty_acceptances(),
        FAST,
    )
    .unwrap();
    assert_eq!(
        plan.merged.environment,
        vec![("PROBE_A".to_string(), "1".to_string())]
    );
    peer.join().unwrap();
}

#[test]
fn prepare_refuses_undeclared_contribution_type() {
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "PROBE_A", "value": "1"}],
    }));
    let policy = defaults();
    run_prepare(
        &mut host,
        "probe",
        &[],
        &MergeContext::empty("/home/cistella"),
        &policy,
        &empty_acceptances(),
        FAST,
    )
    .unwrap_err();
    peer.join().unwrap();
}

#[test]
fn prepare_refuses_unknown_wire_fields_whole() {
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "PROBE_A", "value": "1", "smuggled": true}],
    }));
    let policy = defaults();
    run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        &MergeContext::empty("/home/cistella"),
        &policy,
        &empty_acceptances(),
        FAST,
    )
    .unwrap_err();
    peer.join().unwrap();
}

#[test]
fn prepare_refuses_token_contribution_without_ack() {
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "GITHUB_TOKEN", "value": "x"}],
    }));
    let policy = defaults();
    let error = run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        &MergeContext::empty("/home/cistella"),
        &policy,
        &empty_acceptances(),
        FAST,
    )
    .unwrap_err();
    assert!(error.to_string().contains("GITHUB_TOKEN"));
    peer.join().unwrap();
}

#[test]
fn prepare_discards_weaker_claim_and_proceeds() {
    // Claim weaker (on-extensions) than the user universal rule with
    // the same pattern: discarded with a diagnostic, rule governs.
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "PROBE_A", "value": "1"}],
        "policy_claims": [{"pattern": "^PROBE_", "severity": "suppressible", "scope": "on-extensions"}],
    }));
    let policy = PolicySet::parse(
        b"format_version = 1\n[[denials]]\npattern = '^PROBE_'\nseverity = 'suppressible'\nscope = 'universal'\n[[acknowledgements]]\nname = 'PROBE_A'\n",
    )
    .unwrap();
    let plan = run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        &MergeContext::empty("/home/cistella"),
        &policy,
        &empty_acceptances(),
        FAST,
    )
    .unwrap();
    assert_eq!(plan.diagnostics.len(), 1);
    assert!(plan.diagnostics[0].contains("discards weakening"));
    peer.join().unwrap();
}

#[test]
fn prepare_refuses_overreaching_claim_whole() {
    // Claim matches nothing contributed: beyond its transaction.
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "PROBE_A", "value": "1"}],
        "policy_claims": [{"pattern": "^ELSEWHERE_", "severity": "suppressible", "scope": "universal"}],
    }));
    let policy = defaults();
    let error = run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        &MergeContext::empty("/home/cistella"),
        &policy,
        &empty_acceptances(),
        FAST,
    )
    .unwrap_err();
    assert!(error.to_string().contains("beyond its transaction"));
    peer.join().unwrap();
}

#[test]
fn prepare_upheld_claim_enforces_with_ack_escape() {
    // Stricter-than-nothing claim becomes the transaction rule:
    // refuses unless the user acknowledged the exact name.
    let payload = json!({
        "environment": [{"name": "PROBE_A", "value": "1"}],
        "policy_claims": [{"pattern": "^PROBE_", "severity": "suppressible", "scope": "universal"}],
    });
    let (mut host, peer) = scripted_peer(payload);
    let policy = defaults();
    run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        &MergeContext::empty("/home/cistella"),
        &policy,
        &empty_acceptances(),
        FAST,
    )
    .unwrap_err();
    peer.join().unwrap();
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "PROBE_A", "value": "1"}],
        "policy_claims": [{"pattern": "^PROBE_", "severity": "suppressible", "scope": "universal"}],
    }));
    let acked =
        PolicySet::parse(b"format_version = 1\n[[acknowledgements]]\nname = 'PROBE_A'\n").unwrap();
    run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        &MergeContext::empty("/home/cistella"),
        &acked,
        &empty_acceptances(),
        FAST,
    )
    .unwrap();
    peer.join().unwrap();
}

#[test]
fn capability_names_parse_and_ignore_unknowns() {
    assert_eq!(
        parse_capability("environment"),
        Some(cistella::framework::contract::Capability::Environment)
    );
    assert_eq!(
        parse_capability("credentials"),
        Some(cistella::framework::contract::Capability::Credentials)
    );
    assert_eq!(parse_capability("time-travel"), None);
    let _ = (Scope::Universal, Severity::Suppressible);
}

/// Resolves the fake-guest example binary: `target/<profile>/examples/`
/// sorted by mtime descending (same selection as the integration peer
/// helper; the duplication is small and documented there).
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
fn pending_hook_delivery_refuses_nonempty() {
    use cistella::framework::registry::STAGED_WRAPPER_GUEST_PATH;
    // Interim 3.1 gate: requested confinement without delivery
    // refuses pre-create rather than running unconfined.
    let hook = singleton_hook(vec![STAGED_WRAPPER_GUEST_PATH.to_string()]);
    let error = refuse_pending_hook_delivery(&[hook]).unwrap_err();
    assert!(
        error.to_string().contains("refusing pre-create"),
        "got: {error}"
    );
    refuse_pending_hook_delivery(&[]).unwrap();
}
