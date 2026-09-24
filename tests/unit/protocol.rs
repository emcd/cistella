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
        .request_stream("await_result", json!({}), Some(FAST), &cancel)
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
fn blocked_write_consumes_budget_instead_of_hanging() {
    // Guest end never reads: the pipe fills and the SEND must time
    // out on budget rather than hang. Recv timeouts while the pipe
    // fills are expected (no peer answers); only a write timeout
    // proves the send path is bounded.
    let (mut host, _peer) = exchange();
    let payload = json!({"pad": "x".repeat(63 * 1024)});
    let mut write_timed_out = false;
    for _ in 0..256 {
        match host.request("fill", payload.clone(), Duration::from_millis(100)) {
            Err(e) if e.to_string() == "protocol: frame write timed out" => {
                write_timed_out = true;
                break;
            }
            Err(_) => continue,
            Ok(_) => continue,
        }
    }
    assert!(write_timed_out, "full pipe must time out the send");
}

#[test]
fn unbounded_stream_serves_await_result_only() {
    // Misuse: any other op with no budget refuses immediately.
    let (mut host, _peer) = exchange();
    let cancel = CancelFlag::default();
    host.request_stream("prepare", json!({}), None, &cancel)
        .unwrap_err();
}

#[test]
fn unbounded_await_collects_until_terminal_or_cancel() {
    use cistella::framework::protocol::AWAIT_RESULT_OP;
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        for _ in 0..2 {
            std::thread::sleep(Duration::from_millis(50));
            respond(
                &mut peer,
                &Envelope {
                    protocol: PROTOCOL_MAJOR,
                    id: envelope.id.clone(),
                    op: envelope.op.clone(),
                    payload: json!({"pending": true}),
                },
                PRE_NEGOTIATION_MAX_FRAME,
            );
        }
        respond(
            &mut peer,
            &Envelope {
                protocol: PROTOCOL_MAJOR,
                id: envelope.id.clone(),
                op: envelope.op.clone(),
                payload: json!({"exit_status": 0}),
            },
            PRE_NEGOTIATION_MAX_FRAME,
        );
    });
    let cancel = CancelFlag::default();
    // No budget: completes on terminal far past any finite timeout.
    let outcome = host
        .request_stream(AWAIT_RESULT_OP, json!({}), None, &cancel)
        .unwrap();
    assert_eq!(outcome.pending, 2);
    peer_thread.join().unwrap();
    // Cancelled unbounded stream detaches with a typed error.
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        std::thread::sleep(Duration::from_secs(30));
        let _ = &envelope;
    });
    let cancel = CancelFlag::default();
    cancel.cancel_with(15);
    host.request_stream(AWAIT_RESULT_OP, json!({}), None, &cancel)
        .unwrap_err();
    drop(peer_thread);
}

#[test]
fn spawn_binds_opened_executable_identity() {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read("/bin/cat").expect("read cat");
    let expected = hex::encode(Sha256::digest(&bytes));
    let host = GuestHost::spawn(Path::new("/bin/cat"), &[], Deadlines::default()).unwrap();
    assert_eq!(host.executable_digest(), expected);
    // Identity and exec share one open description: no path check
    // could race this digest.
}

#[test]
fn fd_holder_survival_reports_residue() {
    if std::process::Command::new("setsid")
        .arg("--help")
        .output()
        .is_err()
    {
        eprintln!("skip: setsid unavailable");
        return;
    }
    // The script exits at once, leaving a reparented sleeper (new
    // session, outside the group kill) holding the stdout pipe open
    // for 5 s. Shutdown must report residue instead of clean success.
    let script = "setsid sleep 5 & exit 0".to_string();
    let mut host = GuestHost::spawn(
        Path::new("/bin/sh"),
        &["-c".to_string(), script],
        Deadlines::default(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let error = host.shutdown().unwrap_err();
    assert!(
        error.to_string().contains("retains protocol FDs"),
        "expected FD-holder residue, got: {error}"
    );
}

#[test]
fn blocked_write_cleanup_kills_and_reaps() {
    let mut host = GuestHost::spawn(
        Path::new("/bin/sleep"),
        &["30".to_string()],
        Deadlines::default(),
    )
    .unwrap();
    let pid = host.pid();
    // Fill the never-read stdin until the send itself times out.
    let payload = json!({"pad": "x".repeat(63 * 1024)});
    let mut write_timed_out = false;
    for n in 0..256u64 {
        let envelope = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: format!("req-{n}"),
            op: "fill".to_string(),
            payload: payload.clone(),
        };
        let deadline = std::time::Instant::now() + Duration::from_millis(100);
        match host.exchange_mut().send(&envelope, deadline) {
            Err(e) if e.to_string() == "protocol: frame write timed out" => {
                write_timed_out = true;
                break;
            }
            Err(_) => break,
            Ok(_) => continue,
        }
    }
    assert!(write_timed_out, "full pipe must time out the send");
    host.shutdown().unwrap();
    let gone = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None);
    assert!(gone.is_err());
}

#[test]
fn drop_without_shutdown_still_reaps() {
    let pid = {
        let host = GuestHost::spawn(
            Path::new("/bin/sleep"),
            &["30".to_string()],
            Deadlines::default(),
        )
        .unwrap();
        host.pid()
    };
    let gone = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None);
    assert!(gone.is_err(), "dropped host must reap its guest");
}

#[test]
fn envelope_tokens_are_grammar_bounded() {
    // Control characters, spaces, and overlong tokens refuse even
    // when the JSON shape is otherwise valid.
    for (op, id) in [
        ("pro\x00be", "req-0"),
        ("pro be", "req-0"),
        ("probe", "req-\n0"),
        ("probe", &"x".repeat(200)),
    ] {
        let body = serde_json::to_vec(&json!({
            "protocol": PROTOCOL_MAJOR,
            "id": id,
            "op": op,
            "payload": {},
        }))
        .unwrap();
        parse_envelope(&body).unwrap_err();
    }
}

#[test]
fn envelope_errors_are_fixed_classes_without_echo() {
    // Malformed, truncated, and shape-violating envelopes map to
    // fixed diagnostics: no guest bytes render, even adversarial ones.
    let evil = "x".repeat(500) + "\nSECRET=hunter2";
    for (body, fixed) in [
        (b"{\"protocol\":".to_vec(), "bad envelope: truncated JSON"),
        (b"{oops".to_vec(), "bad envelope: malformed JSON"),
        (
            format!("{{\"protocol\":1,\"id\":\"a\",\"op\":\"o\",\"payload\":null,\"smuggled\":\"{evil}\"}}").into_bytes(),
            "bad envelope: shape violation",
        ),
    ] {
        let error = parse_envelope(&body).unwrap_err().to_string();
        assert!(error.contains(fixed), "got: {error}");
        assert!(!error.contains("hunter2"), "guest content leaked: {error}");
    }
}

#[test]
fn correlation_refusal_names_no_guest_string() {
    let (mut host, mut peer) = exchange();
    let peer_thread = std::thread::spawn(move || {
        let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
        let envelope = parse_envelope(&request).unwrap();
        // Grammar-valid but wrong ID: refusal must not echo it.
        let response = Envelope {
            protocol: PROTOCOL_MAJOR,
            id: "someone-else".to_string(),
            op: envelope.op.clone(),
            payload: json!({"ok": true}),
        };
        respond(&mut peer, &response, PRE_NEGOTIATION_MAX_FRAME);
    });
    let error = host
        .request("probe", json!({}), FAST)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("someone-else"), "id echoed: {error}");
    assert!(error.contains("unknown or duplicate id"), "got: {error}");
    peer_thread.join().unwrap();
}

#[test]
fn unbounded_await_survives_idle_silence() {
    use cistella::framework::protocol::AWAIT_RESULT_OP;
    // Quiet peer: 800 ms of total silence (past the 500 ms poll
    // slice), then a terminal result. An unbounded await must ride
    // through the silence; a finite budget must refuse.
    for timeout in [None, Some(Duration::from_millis(300))] {
        let (mut host, mut peer) = exchange();
        let peer_thread = std::thread::spawn(move || {
            let request = read_frame(&mut peer, PRE_NEGOTIATION_MAX_FRAME, FAST).unwrap();
            let envelope = parse_envelope(&request).unwrap();
            std::thread::sleep(Duration::from_millis(800));
            respond(
                &mut peer,
                &Envelope {
                    protocol: PROTOCOL_MAJOR,
                    id: envelope.id.clone(),
                    op: envelope.op.clone(),
                    payload: json!({"exit_status": 0}),
                },
                PRE_NEGOTIATION_MAX_FRAME,
            );
        });
        let cancel = CancelFlag::default();
        let result = host.request_stream(AWAIT_RESULT_OP, json!({}), timeout, &cancel);
        match timeout {
            None => {
                let outcome = result.unwrap();
                assert_eq!(outcome.result, json!({"exit_status": 0}));
            }
            Some(_) => {
                result.unwrap_err();
            }
        }
        peer_thread.join().unwrap();
    }
}

#[test]
fn unbounded_quiet_cancel_detaches_not_times_out() {
    use cistella::framework::protocol::AWAIT_RESULT_OP;
    // No frames at all; cancellation at 200 ms must surface as
    // cancellation, never as a read timeout.
    let (mut host, _peer) = exchange();
    let cancel = CancelFlag::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            cancel.cancel_with(15);
        });
        let error = host
            .request_stream(AWAIT_RESULT_OP, json!({}), None, &cancel)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("cancelled"),
            "quiet cancel must detach, got: {error}"
        );
    });
}

#[test]
fn post_exit_group_kill_reaches_left_behind_sleepers() {
    // Script quits at once leaving a SAME-GROUP sleeper that would
    // write a marker after 2 s. Shutdown signals the group even
    // though the leader already exited, so the marker never appears.
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker").to_string_lossy().to_string();
    let script = format!("(sleep 2 && touch {marker}) & exit 0");
    let mut host = GuestHost::spawn(
        Path::new("/bin/sh"),
        &["-c".to_string(), script],
        Deadlines::default(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    host.shutdown().unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !dir.path().join("marker").exists(),
        "post-exit group kill must preempt the left-behind sleeper"
    );
}

#[test]
fn group_escapee_with_closed_fds_outside_enforcement() {
    // Narrowed-contract pin: a descendant that escapes its process
    // group AND closes every protocol FD is outside enforcement.
    // Shutdown reports clean (pipes EOF) while the escapee
    // demonstrably survives (marker appears). Pinned helpers are
    // trusted code; this combination requires deliberate evasion.
    // The setsid guard skips only when the binary is absent; the
    // boundary pinned here is the contract, not setsid availability.
    if std::process::Command::new("setsid")
        .arg("--help")
        .output()
        .is_err()
    {
        eprintln!("skip: setsid unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker").to_string_lossy().to_string();
    let script = format!("setsid sh -c 'exec >/dev/null 2>&1; sleep 2; touch {marker}' & exit 0");
    let mut host = GuestHost::spawn(
        Path::new("/bin/sh"),
        &["-c".to_string(), script],
        Deadlines::default(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    host.shutdown().unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        dir.path().join("marker").exists(),
        "escapee survival pins the enforcement boundary"
    );
}

#[test]
fn concurrent_guests_coexist() {
    // The old process-global gate is gone: two live guests hello
    // and round-trip independently, proving the protocol supports
    // multiple extensions without serialization.
    let mut first = GuestHost::spawn(Path::new("/bin/cat"), &[], Deadlines::default()).unwrap();
    let mut second = GuestHost::spawn(Path::new("/bin/cat"), &[], Deadlines::default()).unwrap();
    for host in [&mut first, &mut second] {
        host.exchange_mut().hello(&[], FAST).unwrap();
        let payload = host
            .exchange_mut()
            .request("probe", json!({"n": 1}), FAST)
            .unwrap();
        assert_eq!(payload, json!({"n": 1}));
    }
    first.shutdown().unwrap();
    second.shutdown().unwrap();
}

#[test]
fn in_group_term_ignorer_cleared_by_kill() {
    // Sane in-group descendant ignoring SIGTERM with closed FDs:
    // pipes EOF (nothing retained) but the group survives TERM, so
    // shutdown must escalate to SIGKILL and report clean ONLY with
    // the group extinct (signal-0 pole from the test).
    let deadlines = Deadlines {
        terminate_grace: Duration::from_secs(2),
        ..Deadlines::default()
    };
    let script = "(trap '' TERM; exec >/dev/null 2>&1; sleep 30) & exit 0".to_string();
    let mut host =
        GuestHost::spawn(Path::new("/bin/sh"), &["-c".to_string(), script], deadlines).unwrap();
    let pgid = host.pid() as i32;
    std::thread::sleep(Duration::from_millis(500));
    host.shutdown().unwrap();
    let group = nix::unistd::Pid::from_raw(-pgid);
    assert!(
        nix::sys::signal::kill(group, None).is_err(),
        "in-group ignorer must be reaped by SIGKILL escalation"
    );
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
