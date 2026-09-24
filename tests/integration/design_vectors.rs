//! Design-vector conformance fixtures (task 3.3).
//!
//! Agentmux-support and SSH serve as design vectors, not migrations:
//! these tests prove the framework's typed contribution schemas can
//! express today's hand-rolled per-profile wiring as prepare
//! transactions through the deterministic peer. Each vector pins a
//! contribution SHAPE (exact env names, socket-mount triple
//! structure, claim lattice position, credential handle kind);
//! fixture paths stay synthetic and exact fleet paths bind at the
//! 4.1 dogfood gate. Ground truth citations:
//!
//! - Agentmux env pair: fleet profiles declare
//!   `environment-acceptances = ['AGENTMUX_BUNDLE',
//!   'AGENTMUX_SESSION']`; relay labels (`agentmux.session`) and the
//!   `~/.config/agentmux` mount target appear in
//!   `tests/unit/runtime.rs`.
//! - SSH triple: `identity::ssh_agent_volume_args` wires
//!   `{socket}:{socket}:ro` plus `SSH_AUTH_SOCK={socket}` from the
//!   profile `credential_surface` Agent slot (never from acceptances,
//!   never ambient); the `seat-socket` credential handle mirrors that
//!   slot's bounded socket path.
//! - Token refusal: the compiled-default push-credential guard and
//!   the never-grandfathered-extension rule live in
//!   `framework::policy`.
//!
//! Happy-path vectors run through the host's real `run_prepare`
//! (merge gate, claim partition, lattice evaluation); the negative
//! vector pins value-free refusal of token-shaped extension env.

use std::collections::HashSet;
use std::time::Duration;

use cistella::framework::contract::Deadlines;
use cistella::framework::policy::PolicySet;
use cistella::framework::prepare::{EvaluatedPlan, run_prepare};
use cistella::framework::protocol::GuestHost;

use super::protocol_peer::peer_path;

/// Compiled defaults via load: an absent file means defaults only.
fn defaults() -> PolicySet {
    let dir = tempfile::tempdir().expect("tempdir");
    PolicySet::load(Some(dir.path())).expect("absent file means defaults")
}

/// Policy with exact-name acknowledgements for `names`.
fn acknowledged(names: &[&str]) -> (tempfile::TempDir, PolicySet) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut text = String::from("format_version = 1\n");
    for name in names {
        text.push_str("[[acknowledgements]]\nname = \"");
        text.push_str(name);
        text.push_str("\"\n");
    }
    std::fs::write(dir.path().join("policies.toml"), text).expect("write policies.toml");
    let policy = PolicySet::load(Some(dir.path())).expect("acknowledgements load");
    (dir, policy)
}

/// Full capability advertisement: one source of truth for every
/// vector's hello negotiation and prepare gate.
const FULL_CAPS: &[&str] = &[
    "environment",
    "mounts",
    "policy-claims",
    "guest-hooks",
    "credentials",
];

/// Capability list owned for `hello`/`run_prepare` call sites.
fn full_caps() -> Vec<String> {
    FULL_CAPS.iter().map(ToString::to_string).collect()
}

/// Negotiate hello with the peer advertising the full capability set.
fn negotiate_full(host: &mut GuestHost<std::process::ChildStdout, std::process::ChildStdin>) {
    host.exchange_mut()
        .hello(&full_caps(), Duration::from_secs(2))
        .expect("hello must succeed against the peer");
}

fn tight_deadlines() -> Deadlines {
    Deadlines {
        hello: Duration::from_secs(2),
        plan: Duration::from_secs(2),
        apply: Duration::from_secs(2),
        terminate_grace: Duration::from_secs(2),
    }
}

/// Drive one vector mode through `run_prepare` with the caller's
/// policy and acceptances; the peer's payload is untrusted input at
/// every step.
fn run_vector(
    mode: &str,
    policy: &PolicySet,
    acceptances: &HashSet<String>,
) -> Result<EvaluatedPlan, cistella::error::CistellaError> {
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &[format!("--mode={mode}")], tight_deadlines())
        .expect("peer must spawn");
    negotiate_full(&mut host);
    let result = run_prepare(
        host.exchange_mut(),
        "vector-guest",
        &full_caps(),
        &cistella::framework::contract::MergeContext::empty("/home/cistella"),
        policy,
        acceptances,
        Duration::from_secs(2),
    );
    let cleanup = host.shutdown();
    assert!(
        cleanup.is_ok(),
        "vector mode `{mode}` cleanup must succeed: {cleanup:?}"
    );
    result
}

#[test]
fn vector_agentmux_prepare_merges() {
    // Relay identity forwarding as a transaction: the fleet
    // acceptance pair merges verbatim, the bus socket triple lands
    // read-only, and the tightening claim over the pair's namespace
    // upholds (both names acknowledged) with no diagnostics.
    let (_dir, policy) = acknowledged(&["AGENTMUX_BUNDLE", "AGENTMUX_SESSION"]);
    let acceptances: HashSet<String> = ["AGENTMUX_BUNDLE", "AGENTMUX_SESSION"]
        .map(String::from)
        .into();
    let plan = run_vector("prepare-vector-agentmux", &policy, &acceptances)
        .expect("agentmux vector must merge");
    assert_eq!(
        plan.merged.environment,
        vec![
            ("AGENTMUX_BUNDLE".to_string(), "seat-7f3a".to_string()),
            ("AGENTMUX_SESSION".to_string(), "session-9c1e".to_string()),
        ],
        "acceptance pair merges verbatim in spine order"
    );
    assert_eq!(plan.merged.mounts.len(), 1, "one bus socket triple");
    let triple = &plan.merged.mounts[0];
    assert_eq!(triple.host_source, "/run/vector/agentmux-bus");
    assert_eq!(triple.container_target, "/run/vector/agentmux-bus");
    assert_eq!(triple.mode, cistella::framework::contract::MountMode::Ro);
    assert!(
        plan.diagnostics.is_empty(),
        "tightening claim upholds silently: {:?}",
        plan.diagnostics
    );
    assert!(
        plan.credentials.is_empty(),
        "agentmux vector carries no credential handles"
    );
}

#[test]
fn vector_agentmux_weakening_claim_discarded() {
    // A claim restating the compiled-default token pattern at a
    // narrower scope is a weakening: partition discards it with a
    // typed diagnostic while the stricter default governs. The
    // transaction still succeeds because the contributed token name
    // carries an exact-name acknowledgement (doctrine: acknowledgment
    // excuses, never overrides — the default still governs).
    let (_dir, policy) = acknowledged(&["GH_TOKEN"]);
    let acceptances: HashSet<String> = HashSet::new();
    let plan = run_vector("prepare-vector-token-weakening", &policy, &acceptances)
        .expect("weakening vector must still merge");
    assert_eq!(
        plan.diagnostics.len(),
        1,
        "exactly one weakening discard, got: {:?}",
        plan.diagnostics
    );
    assert!(
        plan.diagnostics[0].contains("discards weakening"),
        "diagnostic names the weakening, got: {}",
        plan.diagnostics[0]
    );
    assert_eq!(
        plan.merged.environment,
        vec![("GH_TOKEN".to_string(), "vector-weakening-probe".to_string())],
        "transaction proceeds under the governing default, excused by acknowledgement"
    );
}

#[test]
fn vector_ssh_prepare_merges() {
    // SSH identity wiring as a transaction: the pointer env and the
    // read-only socket bind merge, and the `seat-socket` handle
    // admits with its locator class (never content). The full
    // capability set is advertised so the handle survives the merge
    // gate — the vector proves credential-seam admission end to end.
    let policy = defaults();
    let acceptances: HashSet<String> = HashSet::new();
    let plan =
        run_vector("prepare-vector-ssh", &policy, &acceptances).expect("ssh vector must merge");
    assert_eq!(
        plan.merged.environment,
        vec![(
            "SSH_AUTH_SOCK".to_string(),
            "/run/cistella/seats/agent.sock".to_string()
        )],
        "socket pointer env merges verbatim"
    );
    assert_eq!(plan.merged.mounts.len(), 1, "one socket triple");
    let triple = &plan.merged.mounts[0];
    assert_eq!(triple.host_source, "/run/cistella/seats/agent.sock");
    assert_eq!(triple.container_target, "/run/cistella/seats/agent.sock");
    assert_eq!(triple.mode, cistella::framework::contract::MountMode::Ro);
    assert_eq!(plan.credentials.len(), 1, "one admitted handle");
    assert_eq!(plan.credentials[0].kind, "seat-socket");
    assert_eq!(plan.credentials[0].locator_class, "seat-socket");
}

#[test]
fn vector_extension_token_refused_value_free() {
    // Negative vector: token-shaped env from extension provenance
    // refuses (never grandfathered) and the diagnostic names the
    // variable without rendering its value. The sentinel value
    // `vector-secret-value` is the canary: any future change that
    // renders it is a value-leak regression.
    let policy = defaults();
    let acceptances: HashSet<String> = HashSet::new();
    let error = run_vector("prepare-vector-token", &policy, &acceptances)
        .expect_err("token-shaped extension env must refuse");
    let message = error.to_string();
    assert!(
        message.contains("GITHUB_TOKEN"),
        "diagnostic names the variable, got: {message}"
    );
    assert!(
        !message.contains("vector-secret-value"),
        "diagnostic must never render the value, got: {message}"
    );
}
