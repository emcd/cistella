//! Protocol host unit tests: framing, negotiation, correlation,
//! streaming, kill semantics, and drain behavior.
//!
//! `Exchange` cases run over socketpairs with scripted peers (no
//! child required); `GuestHost` cases spawn real stub executables
//! (`cat`, `sleep`, `sh`) as fake guests.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use cistella::framework::contract::{CancelFlag, Deadlines};
use cistella::framework::protocol::{
    DEFAULT_MAX_FRAME, Envelope, Exchange, GuestHost, HOST_MAX_FRAME, HelloResponse,
    PRE_NEGOTIATION_MAX_FRAME, PROTOCOL_MAJOR, envelope_bytes, parse_envelope, read_frame,
    write_frame,
};

const FAST: Duration = Duration::from_secs(3);

fn pair() -> (UnixStream, UnixStream) {
    UnixStream::pair().expect("socketpair")
}

/// Host exchange plus the raw peer end for scripting.
fn exchange() -> (Exchange<UnixStream, UnixStream>, UnixStream) {
    let (a, b) = pair();
    let host = Exchange::new(a.try_clone().expect("clone"), a);
    (host, b)
}

fn hello_response(version: u32, max_frame: Option<u32>) -> Value {
    serde_json::to_value(HelloResponse {
        version,
        capabilities: vec!["test-cap".to_string()],
        max_frame,
    })
    .expect("hello serializes")
}

fn respond(peer: &mut UnixStream, envelope: &Envelope, max: usize) {
    write_frame(peer, &envelope_bytes(envelope), max).expect("peer write");
}

#[test]
fn frame_round_trip() {
    let (mut reader, mut writer) = pair();
    write_frame(&mut writer, b"hello-bytes", 1024).unwrap();
    let body = read_frame(&mut reader, 1024, FAST).unwrap();
    assert_eq!(body, b"hello-bytes");
}

#[test]
fn frame_oversize_refuses_before_allocation() {
    let (mut reader, mut writer) = pair();
    // Declared length far above the ceiling with no body behind it:
    // refusal must arrive without waiting for (or allocating) the bytes.
    writer.write_all(&u32::MAX.to_be_bytes()).unwrap();
    writer.flush().unwrap();
    let error = read_frame(&mut reader, 64, FAST).unwrap_err();
    assert!(error.to_string().contains("exceeds maximum"));
}

#[test]
fn frame_truncation_and_eof_refuse() {
    // EOF with zero bytes: header incomplete.
    let (mut reader, writer) = pair();
    drop(writer);
    read_frame(&mut reader, 1024, FAST).unwrap_err();
    // Declared body longer than delivered, then EOF.
    let (mut reader, mut writer) = pair();
    writer.write_all(&8u32.to_be_bytes()).unwrap();
    writer.write_all(b"abc").unwrap();
    drop(writer);
    let error = read_frame(&mut reader, 1024, FAST).unwrap_err();
    assert!(error.to_string().contains("truncated"));
}

#[test]
fn envelope_unknown_fields_refuse() {
    let body = br#"{"protocol":1,"id":"a","op":"o","payload":null,"smuggled":true}"#;
    let error = parse_envelope(body).unwrap_err();
    assert!(error.to_string().contains("bad envelope"));
}

#[test]
fn hello_negotiates_and_clamps() {
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        assert_eq!(envelope.op, "hello");
        let response = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: "hello".to_string(),
            op: "hello".to_string(),
            payload: hello_response(PROTOCOL_MAJOR, Some(u32::MAX)),
        };
        respond(&mut peer, &response, PRE_NEGOTIATION_MAX_FRAME);
    });
    let negotiated = host.hello(&["host-cap".to_string()], FAST).unwrap();
    assert_eq!(negotiated.capabilities, vec!["test-cap".to_string()]);
    assert_eq!(negotiated.max_frame, HOST_MAX_FRAME);
    peer_thread.join().unwrap();
}

#[test]
fn hello_version_mismatch_refuses() {
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        let response = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: envelope.id.clone(),
            op: "hello".to_string(),
            payload: hello_response(PROTOCOL_MAJOR + 1, None),
        };
        respond(&mut peer, &response, PRE_NEGOTIATION_MAX_FRAME);
    });
    let error = host.hello(&[], FAST).unwrap_err();
    assert!(error.to_string().contains("unsupported guest version"));
    peer_thread.join().unwrap();
}

#[test]
fn request_correlates_and_rejects_unknown_id() {
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        // Wrong ID: host must refuse, not deliver.
        let response = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: "someone-else".to_string(),
            op: envelope.op.clone(),
            payload: json!({"ok": true}),
        };
        respond(&mut peer, &response, PRE_NEGOTIATION_MAX_FRAME);
    });
    let error = host.request("probe", json!({}), FAST).unwrap_err();
    assert!(error.to_string().contains("unknown or duplicate id"));
    peer_thread.join().unwrap();
}

#[test]
fn pending_on_solo_exchange_refuses() {
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        let response = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: envelope.id.clone(),
            op: envelope.op.clone(),
            payload: json!({"pending": true}),
        };
        respond(&mut peer, &response, PRE_NEGOTIATION_MAX_FRAME);
    });
    let error = host.request("probe", json!({}), FAST).unwrap_err();
    assert!(error.to_string().contains("pending on a single-shot"));
    peer_thread.join().unwrap();
}

#[test]
fn stream_collects_pending_then_terminal() {
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        for _ in 0..2 {
            let pending = Envelope {
                protocol: PROTOCOL_MAJOR,
                id: envelope.id.clone(),
                op: envelope.op.clone(),
                payload: json!({"pending": true}),
            };
            respond(&mut peer, &pending, PRE_NEGOTIATION_MAX_FRAME);
        }
        let terminal = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: envelope.id.clone(),
            op: envelope.op.clone(),
            payload: json!({"exit_status": 0}),
        };
        respond(&mut peer, &terminal, PRE_NEGOTIATION_MAX_FRAME);
    });
    let cancel = CancelFlag::default();
    let outcome = host
        .request_stream("await_result", json!({}), FAST, &cancel)
        .unwrap();
    assert_eq!(outcome.pending, 2);
    assert_eq!(outcome.result, json!({"exit_status": 0}));
    peer_thread.join().unwrap();
}

#[test]
fn guest_going_off_protocol_after_hello_refuses() {
    // Adversarial drift: a valid hello followed by non-envelope
    // bytes mid-conversation. Each corruption class refuses with a
    // typed protocol error, never a half-read exchange.
    for raw in [
        b"not json at all".to_vec(),
        br#"[1, 2, 3]"#.to_vec(),
        br#"{"protocol":1,"id":"req-0","op":"prepare"}"#.to_vec(),
    ] {
        let (mut host, mut peer) = exchange();
        write_frame(
            &mut peer,
            &envelope_bytes(&Envelope {
                protocol: PROTOCOL_MAJOR,
                id: "hello".to_string(),
                op: "hello".to_string(),
                payload: hello_response(PROTOCOL_MAJOR, None),
            }),
            PRE_NEGOTIATION_MAX_FRAME,
        )
        .unwrap();
        host.hello(&[], FAST).unwrap();
        write_frame(&mut peer, &raw, PRE_NEGOTIATION_MAX_FRAME).unwrap();
        host.request("prepare", json!({}), FAST).unwrap_err();
    }
}

#[test]
fn guest_executable_must_be_absolute() {
    let error = match GuestHost::spawn(Path::new("relative/guest"), &[], Deadlines::default()) {
        Err(error) => error,
        Ok(_) => panic!("relative guest path must refuse"),
    };
    assert!(error.to_string().contains("must be absolute"));
}

#[test]
fn cat_loopback_negotiates_and_round_trips() {
    // `cat` echoes requests: the echo carries the live ID with a
    // non-pending payload, so hello negotiates (echoed HelloRequest
    // parses as HelloResponse with version 1) and solo requests
    // round-trip their payload.
    let mut host = GuestHost::spawn(Path::new("/bin/cat"), &[], Deadlines::default()).unwrap();
    let negotiated = host
        .exchange_mut()
        .hello(&["host-cap".to_string()], FAST)
        .unwrap();
    assert_eq!(negotiated.max_frame, DEFAULT_MAX_FRAME);
    let payload = host
        .exchange_mut()
        .request("probe", json!({"n": 1}), FAST)
        .unwrap();
    assert_eq!(payload, json!({"n": 1}));
    host.shutdown().unwrap();
}

#[test]
fn slow_guest_times_out_and_reaps() {
    let mut host = GuestHost::spawn(
        Path::new("/bin/sleep"),
        &["30".to_string()],
        Deadlines::default(),
    )
    .unwrap();
    let pid = host.pid();
    let error = host
        .exchange_mut()
        .request("probe", json!({}), Duration::from_millis(300))
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    host.shutdown().unwrap();
    // Reaped: signalling the pid fails with ESRCH.
    let gone = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None);
    assert!(gone.is_err());
}

#[test]
fn stderr_flood_does_not_deadlock() {
    // Without the background drain, 200 KB to a 64 KB pipe would wedge
    // the guest before it ever echoes; with it, hello completes.
    let script = "head -c 200000 /dev/zero >&2; exec cat".to_string();
    let mut host = GuestHost::spawn(
        Path::new("/bin/sh"),
        &["-c".to_string(), script],
        Deadlines::default(),
    )
    .unwrap();
    host.exchange_mut().hello(&[], FAST).unwrap();
    host.shutdown().unwrap();
}
