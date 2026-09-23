//! Credential seam unit tests: handle grammars, locality,
//! unknown-shape refusal, diagnostic redaction, and transaction
//! atomicity. No real credential transport anywhere: the fake proves
//! shapes, never secrets.

use std::collections::HashSet;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::json;

use cistella::framework::credentials::{admit_all, parse_wire};
use cistella::framework::policy::PolicySet;
use cistella::framework::prepare::run_prepare;
use cistella::framework::protocol::{
    Envelope, Exchange, PRE_NEGOTIATION_MAX_FRAME, PROTOCOL_MAJOR, envelope_bytes, parse_envelope,
    read_frame, write_frame,
};

const FAST: Duration = Duration::from_secs(3);

fn full_caps() -> Vec<String> {
    [
        "environment",
        "mounts",
        "policy-claims",
        "guest-hooks",
        "credentials",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[test]
fn opaque_reference_admits_minted_shape() {
    let handles = parse_wire(&json!([
        {"kind": "opaque-reference", "id": "abc123"},
    ]))
    .unwrap();
    let admitted = admit_all(&handles).unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].kind, "opaque-reference");
    assert_eq!(admitted[0].locator_class, "opaque-id");
}

#[test]
fn opaque_reference_refuses_user_chosen_strings() {
    for id in ["ABC123", "has space", "has/slash", "UPPER", ""] {
        let handles = parse_wire(&json!([
            {"kind": "opaque-reference", "id": id},
        ]))
        .unwrap();
        admit_all(&handles).unwrap_err();
    }
    let long = "a".repeat(2048);
    let handles = parse_wire(&json!([
        {"kind": "opaque-reference", "id": long},
    ]))
    .unwrap();
    admit_all(&handles).unwrap_err();
}

#[test]
fn seat_socket_admits_bounded_prefix() {
    let handles = parse_wire(&json!([
        {"kind": "seat-socket", "path": "/run/cistella/seats/agent-7.sock"},
    ]))
    .unwrap();
    let admitted = admit_all(&handles).unwrap();
    assert_eq!(admitted[0].kind, "seat-socket");
}

#[test]
fn seat_socket_refuses_escape_and_shape() {
    for path in [
        "/etc/shadow",
        "/run/cistella/seats/../escape.sock",
        "/run/cistella/seats/sub/../escape.sock",
        "relative.sock",
        "/run/cistella/seats/",
        "/run/cistella/seats/has space.sock",
        "/run/cistella/seats",
    ] {
        let handles = parse_wire(&json!([
            {"kind": "seat-socket", "path": path},
        ]))
        .unwrap();
        admit_all(&handles).unwrap_err();
    }
}

#[test]
fn unknown_variants_and_fields_refuse() {
    // Invented kind.
    parse_wire(&json!([
        {"kind": "sesame-open", "value": "hunter2"},
    ]))
    .unwrap_err();
    // Value-shaped channel does not exist.
    parse_wire(&json!([
        {"kind": "opaque-reference", "id": "abc123", "value": "hunter2"},
    ]))
    .unwrap_err();
    // Smuggled field on a known variant.
    parse_wire(&json!([
        {"kind": "seat-socket", "path": "/run/cistella/seats/a.sock", "extra": 1},
    ]))
    .unwrap_err();
    // Non-array top level.
    parse_wire(&json!({"kind": "opaque-reference", "id": "abc123"})).unwrap_err();
}

#[test]
fn diagnostics_name_kind_never_content() {
    let secret_path = "/etc/shadow";
    let handles = parse_wire(&json!([
        {"kind": "seat-socket", "path": secret_path},
    ]))
    .unwrap();
    let error = admit_all(&handles).unwrap_err().to_string();
    assert!(error.contains("seat-socket"));
    assert!(!error.contains(secret_path));
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

#[test]
fn prepare_admits_handles_with_capability() {
    let (mut host, peer) = scripted_peer(json!({
        "environment": [],
        "credentials": [{"kind": "opaque-reference", "id": "abc123"}],
    }));
    let policy = PolicySet::parse(b"").unwrap();
    let plan = run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        "/home/cistella",
        &policy,
        &HashSet::new(),
        FAST,
    )
    .unwrap();
    assert_eq!(plan.credentials.len(), 1);
    peer.join().unwrap();
}

#[test]
fn prepare_refuses_handles_without_capability() {
    let (mut host, peer) = scripted_peer(json!({
        "credentials": [{"kind": "opaque-reference", "id": "abc123"}],
    }));
    let policy = PolicySet::parse(b"").unwrap();
    run_prepare(
        &mut host,
        "probe",
        &["environment".to_string()],
        "/home/cistella",
        &policy,
        &HashSet::new(),
        FAST,
    )
    .unwrap_err();
    peer.join().unwrap();
}

#[test]
fn prepare_bad_handle_fails_whole_transaction() {
    let (mut host, peer) = scripted_peer(json!({
        "environment": [{"name": "PROBE_A", "value": "1"}],
        "credentials": [{"kind": "seat-socket", "path": "/etc/shadow"}],
    }));
    let policy = PolicySet::parse(b"").unwrap();
    run_prepare(
        &mut host,
        "probe",
        &full_caps(),
        "/home/cistella",
        &policy,
        &HashSet::new(),
        FAST,
    )
    .unwrap_err();
    peer.join().unwrap();
}
