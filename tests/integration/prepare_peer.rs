//! Prepare-transaction conformance via the deterministic peer (task 3.1,
//! prepare-fault bucket).
//!
//! Each test drives one prepare fault against the host's
//! `framework::prepare::run_prepare`:
//!   - spawn the peer in the relevant `--mode=`
//!   - negotiate hello with the full capability set advertised
//!   - call `run_prepare` with empty acceptances and a default
//!     `PolicySet`
//!   - assert typed `Contract` refusal
//!
//! The peer's payload shapes match the frozen 2.2 wire (`PrepareResponse`
//! in `src/framework/prepare.rs`). The host's merge gate,
//! `check_claims`, and `order_hooks` each own one fault class.

use std::collections::HashSet;
use std::time::Duration;

use cistella::framework::contract::Deadlines;
use cistella::framework::policy::PolicySet;
use cistella::framework::prepare::run_prepare;
use cistella::framework::protocol::GuestHost;

use super::protocol_peer::peer_path;

/// Compiled defaults via load: an absent file means defaults only.
fn defaults() -> PolicySet {
    let dir = tempfile::tempdir().expect("tempdir");
    PolicySet::load(Some(dir.path())).expect("absent file means defaults")
}

/// Negotiate hello with the peer advertising the full capability set.
fn negotiate_full(host: &mut GuestHost<std::process::ChildStdout, std::process::ChildStdin>) {
    host.exchange_mut()
        .hello(
            &[
                "environment".to_string(),
                "mounts".to_string(),
                "policy-claims".to_string(),
                "guest-hooks".to_string(),
                "credentials".to_string(),
            ],
            Duration::from_secs(2),
        )
        .expect("hello must succeed against the peer");
}

/// Run prepare with the full capability advertisement, default policy,
/// empty acceptances, and the container_home the merge gate expects.
fn run_full_prepare(
    host: &mut GuestHost<std::process::ChildStdout, std::process::ChildStdin>,
) -> Result<cistella::framework::prepare::EvaluatedPlan, cistella::error::CistellaError> {
    let policy = defaults();
    let acceptances: HashSet<String> = HashSet::new();
    run_prepare(
        host.exchange_mut(),
        "test-guest",
        &[
            "environment".to_string(),
            "mounts".to_string(),
            "policy-claims".to_string(),
            "guest-hooks".to_string(),
            "credentials".to_string(),
        ],
        &cistella::framework::contract::MergeContext::empty("/home/cistella"),
        &policy,
        &acceptances,
        Duration::from_secs(2),
    )
}

fn tight_deadlines() -> Deadlines {
    Deadlines {
        hello: Duration::from_secs(2),
        plan: Duration::from_secs(2),
        apply: Duration::from_secs(2),
        terminate_grace: Duration::from_secs(2),
        frame_completion: Duration::from_secs(60),
    }
}

fn run_fault(mode: &str) -> cistella::error::CistellaError {
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &[format!("--mode={mode}")], tight_deadlines())
        .expect("peer must spawn");
    negotiate_full(&mut host);
    let result = run_full_prepare(&mut host);
    let cleanup = host.shutdown();
    assert!(
        cleanup.is_ok(),
        "fault mode `{mode}` cleanup must succeed: {cleanup:?}"
    );
    result.expect_err(&format!("fault mode `{mode}` must produce an Err; got Ok"))
}

#[test]
fn prepare_unadvertised_capability_refuses() {
    // Peer advertises empty capabilities (in hello) and returns a
    // non-empty `environment`. The host's `merge_prepare` refuses
    // undeclared contribution types BEFORE any shape check.
    //
    // (Note: the wire-level `run_prepare` here passes a full
    // capability set to drive the merge gate; the per-guest
    // advertisement mismatch is encoded in the peer's hello response
    // — for this test we use a NEGATIVE capability list to exercise
    // the gate.)
    use cistella::framework::prepare::run_prepare;
    let path = peer_path();
    let mut host = GuestHost::spawn(
        &path,
        &["--mode=prepare-unadvertised-capability".to_string()],
        tight_deadlines(),
    )
    .expect("peer must spawn");
    host.exchange_mut()
        .hello(&[], Duration::from_secs(2))
        .expect("hello with empty capabilities");
    let policy = defaults();
    let acceptances: HashSet<String> = HashSet::new();
    let result = run_prepare(
        host.exchange_mut(),
        "test-guest",
        &[], // <-- no capabilities advertised
        &cistella::framework::contract::MergeContext::empty("/home/cistella"),
        &policy,
        &acceptances,
        Duration::from_secs(2),
    );
    let cleanup = host.shutdown();
    let error = result.expect_err("unadvertised capability must refuse");
    let message = error.to_string();
    assert!(
        message.contains("unadvertised") || message.contains("contribution type"),
        "expected unadvertised-contribution error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn prepare_duplicate_env_name_refuses() {
    let error = run_fault("prepare-duplicate-env-name");
    let message = error.to_string();
    assert!(
        message.contains("duplicate environment contribution"),
        "expected duplicate-env error, got: {message}"
    );
}

#[test]
fn prepare_mount_violation_refuses() {
    let error = run_fault("prepare-mount-violation");
    let message = error.to_string();
    assert!(
        message.contains("host_source must not contain") || message.contains("="),
        "expected mount-shape error, got: {message}"
    );
}

#[test]
fn prepare_malformed_policy_claim_refuses() {
    let error = run_fault("prepare-malformed-policy-claim");
    let message = error.to_string();
    assert!(
        message.contains("policy claim pattern must not be empty"),
        "expected empty-pattern error, got: {message}"
    );
}

#[test]
fn prepare_duplicate_hook_order_refuses() {
    let error = run_fault("prepare-duplicate-hook-order");
    let message = error.to_string();
    assert!(
        message.contains("duplicate guest hook order"),
        "expected duplicate-hook-order error, got: {message}"
    );
}

#[test]
fn prepare_credential_shape_fault_refuses() {
    // Peer smuggles a `credentials` contribution carrying a raw
    // `value` field (forbidden by the schema-level secret
    // unrepresentability rule). Post-2.3, `credentials` is a
    // first-class typed contribution; the gate that refuses this
    // payload is the credential-handle schema's strict
    // `deny_unknown_fields` (or the equivalent typed-shape
    // refusal), NOT the outer `PrepareResponse` deny. The wire
    // error surface is "bad prepare response" or a typed Contract
    // refusal naming the unknown field.
    let error = run_fault("prepare-credential-shape-fault");
    let message = error.to_string();
    assert!(
        message.contains("bad prepare response")
            || message.contains("unknown")
            || message.contains("credentials")
            || message.contains("secret"),
        "expected credential-shape refusal, got: {message}"
    );
}
