//! Prepare-transaction fault modes for the deterministic protocol peer.
//!
//! Grouped from the single-file peer at the file-size limit; wire
//! shapes unchanged. See `main.rs` for shared framing helpers.

use std::io::{StdinLock, StdoutLock};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::{PROTOCOL_MAJOR, hello_response_ok, read_frame_from_stdin, write_frame};

/// Runs one Prepare-transaction mode; `None` falls through to the next group.
pub(crate) fn run(
    mode: &str,
    stdin_lock: &mut StdinLock<'_>,
    stdout_lock: &mut StdoutLock<'_>,
) -> Option<ExitCode> {
    Some(match mode {
        "prepare-unadvertised-capability" => {
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [
                        {"name": "PROBE_LEAK", "value": "x"}
                    ],
                    "mounts": [],
                    "policy_claims": [],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-duplicate-env-name" => {
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [
                        {"name": "DUPLICATE_VAR", "value": "first"},
                        {"name": "DUPLICATE_VAR", "value": "second"}
                    ],
                    "mounts": [],
                    "policy_claims": [],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-mount-violation" => {
            // host_source contains `=` — `validate_mounts` refuses
            // with "host_source must not contain control/'='".
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [],
                    "mounts": [
                        {
                            "host_source": "/bad=source",
                            "container_target": "/work",
                            "mode": "rw"
                        }
                    ],
                    "policy_claims": [],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-malformed-policy-claim" => {
            // Empty pattern — `check_claims` refuses.
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [],
                    "mounts": [],
                    "policy_claims": [
                        {
                            "pattern": "",
                            "severity": "suppressible",
                            "scope": "universal"
                        }
                    ],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-duplicate-hook-order" => {
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [],
                    "mounts": [],
                    "policy_claims": [],
                    "guest_hooks": [
                        {"order": 0, "argv_prefix": ["a"], "probe_op": "noop"},
                        {"order": 0, "argv_prefix": ["b"], "probe_op": "noop"}
                    ]
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-credential-shape-fault" => {
            // Schema-level shape fault: a credential contribution that
            // carries a forbidden `value` field. The host's
            // `deny_unknown_fields` (or shape refusal for
            // credentials) refuses before any merge. Because the
            // prepare wire schema does NOT yet include a credentials
            // typed contribution in the public response, this mode
            // smuggles a `credentials` array with raw `value`
            // fields; deny_unknown_fields refuses the unknown key.
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [],
                    "mounts": [],
                    "policy_claims": [],
                    "guest_hooks": [],
                    "credentials": [
                        {"handle": "/run/creds/x", "value": "raw-secret"}
                    ]
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-empty-claim-pattern" => {
            // Same shape as `prepare-malformed-policy-claim` (empty
            // pattern). Kept separate so the harness enumerates the
            // fault explicitly; if 2.2 adds another shape dimension,
            // the two cases diverge.
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [],
                    "mounts": [],
                    "policy_claims": [],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        // ---- Design-vector happy paths (task 3.3): realistic
        //      fleet-wiring shapes proving the typed contribution
        //      schemas express today's per-profile wiring. Paths are
        //      fixture-synthetic; the pinned shapes are the env-name
        //      pair, the socket-mount triple structure, and the claim
        //      lattice position. No migration: exact fleet paths bind
        //      at the 4.1 dogfood gate.
        "prepare-vector-agentmux" => {
            // Relay identity forwarding as a prepare transaction:
            // the `AGENTMUX_BUNDLE`/`AGENTMUX_SESSION` exact-name pair
            // (fleet `environment-acceptances` ground truth) plus a
            // relay-bus socket mount plus a tightening claim over the
            // pair's namespace.
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [
                        {"name": "AGENTMUX_BUNDLE", "value": "seat-7f3a"},
                        {"name": "AGENTMUX_SESSION", "value": "session-9c1e"}
                    ],
                    "mounts": [
                        {
                            "host_source": "/run/vector/agentmux-bus",
                            "container_target": "/run/vector/agentmux-bus",
                            "mode": "ro"
                        }
                    ],
                    "policy_claims": [
                        {
                            "pattern": "AGENTMUX_.*",
                            "severity": "suppressible",
                            "scope": "on-extensions"
                        }
                    ],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-vector-ssh" => {
            // SSH identity wiring as a prepare transaction: the
            // `SSH_AUTH_SOCK` pointer env plus the per-seat socket
            // bind read-only (mirrors
            // `identity::ssh_agent_volume_args`) plus the
            // `seat-socket` credential handle for the same socket
            // (mirrors the profile `credential_surface` Agent slot).
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [
                        {
                            "name": "SSH_AUTH_SOCK",
                            "value": "/run/cistella/seats/agent.sock"
                        }
                    ],
                    "mounts": [
                        {
                            "host_source": "/run/cistella/seats/agent.sock",
                            "container_target": "/run/cistella/seats/agent.sock",
                            "mode": "ro"
                        }
                    ],
                    "policy_claims": [],
                    "guest_hooks": [],
                    "credentials": [
                        {
                            "kind": "seat-socket",
                            "path": "/run/cistella/seats/agent.sock"
                        }
                    ]
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-vector-token" => {
            // Negative vector: a token-shaped env name contributed by
            // an extension. The compiled-default push-credential guard
            // refuses it (extension provenance is never
            // grandfathered); the diagnostic must name the variable
            // without rendering its value.
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [
                        {"name": "GITHUB_TOKEN", "value": "vector-secret-value"}
                    ],
                    "mounts": [],
                    "policy_claims": [],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        "prepare-vector-token-weakening" => {
            // Weakening vector: a claim restating the
            // compiled-default token pattern at a narrower scope
            // (`on-extensions` vs the default's `universal`).
            // Partition must discard it with a diagnostic while the
            // stricter default governs; the transaction proceeds
            // because the contributed token name is acknowledged.
            send_prepare_with_payload(
                stdin_lock,
                stdout_lock,
                json!({
                    "environment": [
                        {"name": "GH_TOKEN", "value": "vector-weakening-probe"}
                    ],
                    "mounts": [],
                    "policy_claims": [
                        {
                            "pattern": "(?i)^(GITHUB_TOKEN|GH_TOKEN|GITHUB_PAT)$",
                            "severity": "suppressible",
                            "scope": "on-extensions"
                        }
                    ],
                    "guest_hooks": []
                }),
            );
            ExitCode::SUCCESS
        }
        _ => return None,
    })
}

/// Drives one prepare transaction: consumes the hello, replies with a
/// valid hello response, consumes the prepare request, then sends a
/// prepare response carrying `payload` as the body. The host's
/// `run_prepare` parses `payload` and applies its existing merge and
/// lattice rules; the test asserts the typed refusal or success.
fn send_prepare_with_payload(
    stdin: &mut std::io::StdinLock<'_>,
    stdout: &mut std::io::StdoutLock<'_>,
    payload: Value,
) {
    if read_frame_from_stdin(stdin).is_err() {
        return;
    }
    let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
    let _ = write_frame(stdout, &body1, 64 * 1024);
    let req_body = match read_frame_from_stdin(stdin) {
        Ok(body) => body,
        Err(_) => return,
    };
    let id = serde_json::from_slice::<Value>(&req_body)
        .ok()
        .and_then(|env| env.get("id").cloned())
        .unwrap_or(json!("req-0"));
    let response = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": id,
        "op": "prepare",
        "payload": payload,
    });
    let body = serde_json::to_vec(&response).expect("serialize");
    let _ = write_frame(stdout, &body, 64 * 1024);
}
