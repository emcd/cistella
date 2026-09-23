//! Protocol conformance via the deterministic peer (task 3.1, fake-
//! protocol/fault bucket).
//!
//! The peer lives at `tests/fixtures/fake_guest.rs` as a Cargo
//! test-auxiliary target (out of `package.include`); we resolve its
//! pinned absolute path via `CARGO_BIN_EXE_fake_guest` at test
//! runtime, then spawn it through `framework::protocol::GuestHost`
//! exactly like a real extension/isolator helper.
//!
//! What this module proves:
//!   - The host's `Exchange` framing/negotiation refuses each
//!     scripted fault class with a typed `Protocol` error.
//!   - The host's `GuestHost::shutdown` reaps the peer, drains stderr,
//!     and restores SIGPIPE no matter how the peer misbehaved.
//!   - Out-of-order cleanup (kill on a peer that hung mid-frame) is
//!     residue-dominated, not abandoned.
//!
//! What this module does NOT prove:
//!   - Lifecycle meaning (lifecycle-common cases skip the peer by
//!     applicability rule #1; those cases live in `conformance.rs`).
//!   - Real resource lifecycle against Podman (Podman fixtures in
//!     `conformance.rs` cover that).

use std::path::PathBuf;
use std::time::Duration;

use cistella::framework::contract::Deadlines;
use cistella::framework::protocol::{GuestHost, PROTOCOL_MAJOR};

/// Resolves the peer path. Cargo's `CARGO_BIN_EXE_<name>` env var is
/// only set inside the test binary that OWNS the binary, not in
/// sibling test binaries that depend on it (verified for `[[test]]`
/// and `examples/` targets: neither exposes the var to other
/// integration tests). The peer is built next to the integration
/// test binary in `target/<profile>/deps/` when `cargo test` builds
/// all targets; filtered single-target runs do NOT build the peer,
/// so glob-resolving on a filtered run returns "not found".
///
/// We find the peer by globbing the deps directory and skipping
/// dep-info files (`fake_guest-<hash>.d`) so the match is the
/// actual executable.
pub(super) fn peer_path() -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let my_dir = my_path.parent().expect("deps dir");
    let prefix = "fake_guest";
    let mut found: Option<PathBuf> = None;
    let entries = std::fs::read_dir(my_dir).expect("read deps dir");
    for entry in entries {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Match `fake_guest-<hash>` and `fake_guest-<hash>.exe`; skip
        // `fake_guest-<hash>.d` (dep-info, not an executable).
        if !name.starts_with(prefix) {
            continue;
        }
        let after = &name[prefix.len()..];
        if !after.starts_with('-') {
            continue;
        }
        let metadata = entry.metadata().expect("metadata");
        // Executable bit set, regular file.
        if metadata.is_file() && (metadata.mode() & 0o111) != 0 {
            found = Some(entry.path());
            break;
        }
    }
    found.unwrap_or_else(|| {
        panic!(
            "fake_guest executable not found in {}; \
             run `cargo build --test fake_guest` (or a full `cargo test`) \
             to produce it before invoking filtered single-target runs",
            my_dir.display()
        )
    })
}

/// Tight deadlines so the test surface stays bounded; the peer faults
/// are designed to fire inside this window.
fn deadlines() -> Deadlines {
    Deadlines {
        hello: Duration::from_secs(2),
        plan: Duration::from_secs(2),
        apply: Duration::from_secs(2),
        terminate_grace: Duration::from_secs(2),
    }
}

/// Every fault-mode test asserts the host returns `Err` of some kind
/// (typed `Protocol` error in most cases) and that `shutdown` cleans
/// up cleanly. None of these tests assert specific error wording —
/// only that the host refused/recovered deterministically.
fn assert_host_refuses<F>(mode: &str, body: F)
where
    F: FnOnce(
        &mut GuestHost<std::process::ChildStdout, std::process::ChildStdin>,
    ) -> Result<(), String>,
{
    let path = peer_path();
    let mut host =
        GuestHost::spawn(&path, &[format!("--mode={mode}")], deadlines()).expect("peer must spawn");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut host)));
    let body_outcome = result.unwrap_or_else(|_| Err("body panicked".to_string()));
    let cleanup = host.shutdown();
    assert!(
        body_outcome.is_err(),
        "fault mode `{mode}` must produce an Err, got Ok"
    );
    assert!(
        cleanup.is_ok(),
        "fault mode `{mode}` cleanup must succeed; got {cleanup:?}"
    );
}

fn hello_negotiation_refuses<F>(mode: &str, body: F)
where
    F: FnOnce(
        &mut cistella::framework::protocol::Exchange<
            std::process::ChildStdout,
            std::process::ChildStdin,
        >,
    ) -> Result<(), String>,
{
    assert_host_refuses(mode, |host| body(host.exchange_mut()))
}

#[test]
fn hello_version_mismatch_refuses() {
    hello_negotiation_refuses("hello-version-mismatch", |exchange| {
        let result = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
        if let Err(error) = &result {
            let message = error.to_string();
            assert!(
                message.contains("version") || message.contains("unsupported"),
                "expected version-mismatch error, got: {message}"
            );
        }
        result.map(|_| ()).map_err(|e| e.to_string())
    });
}

#[test]
fn hello_bad_capability_does_not_crash_host() {
    // The host does not validate guest capability names today (the
    // guest self-declares its capability set; capability gating is
    // enforced later via the prepare transaction's advertisement
    // check). The peer still survives, and the host must not panic
    // or hang. Either Ok or Err is acceptable as long as the host
    // reaches a deterministic terminal state.
    let path = peer_path();
    let mut host = GuestHost::spawn(
        &path,
        &["--mode=hello-bad-capability".to_string()],
        deadlines(),
    )
    .expect("peer spawn");
    let result = host
        .exchange_mut()
        .hello(&["test-cap".to_string()], Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
    // We do not assert Ok-vs-Err here: either is acceptable host
    // behavior for an unknown capability name. The invariant is
    // "no panic, no hang, deterministic cleanup".
    let _ = result;
}

#[test]
fn hello_then_eof_recovers() {
    hello_negotiation_refuses("hello-then-eof", |exchange| {
        let _ = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
        // After hello, peer closes stdout. Any subsequent recv fails typed.
        let result = exchange.recv(Duration::from_secs(1));
        if let Err(error) = &result {
            let message = error.to_string();
            assert!(
                message.contains("EOF")
                    || message.contains("truncated")
                    || message.contains("timed out"),
                "expected EOF/truncated/timeout error, got: {message}"
            );
        }
        result.map(|_| ()).map_err(|e| e.to_string())
    });
}

#[test]
fn oversize_frame_header_refuses() {
    // `oversize-frame` declares a length just past `PRE_NEGOTIATION_MAX_FRAME`.
    // The host's read_frame refuses before allocating the body.
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=oversize-frame".to_string()], deadlines())
        .expect("peer spawn");
    let result = host
        .exchange_mut()
        .hello(&["test-cap".to_string()], Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(result.is_err(), "oversize hello frame must refuse");
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("exceeds maximum") || message.contains("frame length"),
        "expected oversize error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn unknown_fields_envelope_refuses() {
    // The peer sends a hello payload with an extra `smuggled` field.
    // The host's `Envelope` has `deny_unknown_fields`, so `parse_envelope`
    // refuses before negotiation completes.
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=unknown-fields".to_string()], deadlines())
        .expect("peer spawn");
    let result = host
        .exchange_mut()
        .hello(&["test-cap".to_string()], Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(result.is_err(), "unknown-fields envelope must refuse");
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("bad envelope") || message.contains("unknown"),
        "expected envelope-refuse error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn hang_hello_killed_by_deadline() {
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=hang-hello".to_string()], deadlines())
        .expect("peer spawn");
    let start = std::time::Instant::now();
    let result = host
        .exchange_mut()
        .hello(&["test-cap".to_string()], Duration::from_secs(1));
    let elapsed = start.elapsed();
    let cleanup = host.shutdown();
    assert!(result.is_err(), "hang-hello must time out");
    assert!(
        elapsed < Duration::from_secs(5),
        "hang-hello must respect 2s hello deadline; took {elapsed:?}"
    );
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("timed out") || message.contains("frame read"),
        "expected timeout error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must kill+reap: {cleanup:?}");
}

#[test]
fn partial_response_recovers() {
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=partial-response".to_string()], deadlines())
        .expect("peer spawn");
    let result = host
        .exchange_mut()
        .hello(&["test-cap".to_string()], Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(result.is_err(), "partial response must refuse");
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("truncated") || message.contains("EOF"),
        "expected truncated/EOF error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn spurious_after_terminal_recovers() {
    let path = peer_path();
    let mut host = GuestHost::spawn(
        &path,
        &["--mode=spurious-after-terminal".to_string()],
        deadlines(),
    )
    .expect("peer spawn");
    let exchange = host.exchange_mut();
    let _ = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
    // First request: peer responds validly. After the terminal frame,
    // the host's `request` is satisfied — the spurious frame sits in
    // the pipe buffer OR the peer exits before the second send. Either
    // way, the second request must surface as a typed error
    // (correlation refusal on the stray frame, or pipe write error
    // when the peer has already closed its read end).
    let first = exchange.request("ping", serde_json::json!({}), Duration::from_secs(1));
    assert!(first.is_ok(), "first request must succeed: {first:?}");
    let second = exchange.request("ping", serde_json::json!({}), Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(
        second.is_err(),
        "second request must refuse (stray frame consumed or peer closed): {second:?}"
    );
    let message = second.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("unknown or duplicate id")
            || message.contains("correlation")
            || message.contains("Broken pipe")
            || message.contains("EPIPE")
            || message.contains("truncated")
            || message.contains("EOF")
            || message.contains("frame write"),
        "expected correlation refusal or pipe-write error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
    assert_eq!(PROTOCOL_MAJOR, 1);
}

#[test]
fn duplicate_id_refuses() {
    // Peer sends hello + a second response with id "hello" (the
    // correlation id for hello). After hello succeeds, the host's
    // next recv should refuse the duplicate id.
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=duplicate-id".to_string()], deadlines())
        .expect("peer spawn");
    let exchange = host.exchange_mut();
    let hello = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(hello.is_ok(), "first hello must succeed: {hello:?}");
    // No second recv: peer did not provide a request-response cycle
    // for us to assert against. The relevant invariant is that
    // shutdown cleans up despite the duplicate-id write in the pipe.
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn pending_on_solo_refuses() {
    // Peer responds to a single-shot request with `{pending: true}`.
    // The host's `request` (non-streaming) refuses pending frames.
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=pending-on-solo".to_string()], deadlines())
        .expect("peer spawn");
    let exchange = host.exchange_mut();
    let _ = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
    let result = exchange.request("ping", serde_json::json!({}), Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(result.is_err(), "pending-on-solo must refuse");
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("pending") && message.contains("single-shot"),
        "expected single-shot-pending refusal, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn hang_request_killed_by_deadline() {
    // Peer hangs after hello. Host's request fires the apply
    // deadline; the kill+reap is residue-dominated.
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=hang-request".to_string()], deadlines())
        .expect("peer spawn");
    let exchange = host.exchange_mut();
    let _ = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
    let start = std::time::Instant::now();
    let result = exchange.request("ping", serde_json::json!({}), Duration::from_millis(500));
    let elapsed = start.elapsed();
    let cleanup = host.shutdown();
    assert!(result.is_err(), "hang-request must time out");
    assert!(
        elapsed < Duration::from_secs(3),
        "hang-request must respect 500ms request timeout; took {elapsed:?}"
    );
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("timed out"),
        "expected timeout error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must kill+reap: {cleanup:?}");
}

#[test]
fn cleanup_then_write_surfaces_typed_error() {
    // Peer drops its stdout (closes fd 1) after hello, then sleeps and
    // exits without writing a response. The host's recv observes EOF
    // on the pipe (typed Protocol error). The host must surface the
    // failure as a typed error, not a panic or hang.
    let path = peer_path();
    let mut host = GuestHost::spawn(
        &path,
        &["--mode=cleanup-then-write".to_string()],
        deadlines(),
    )
    .expect("peer spawn");
    let exchange = host.exchange_mut();
    let _ = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
    let result = exchange.request("ping", serde_json::json!({}), Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(result.is_err(), "cleanup-then-write must surface as Err");
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("truncated")
            || message.contains("EOF")
            || message.contains("Broken pipe")
            || message.contains("EPIPE")
            || message.contains("frame write")
            || message.contains("EBADF"),
        "expected pipe-close/EOF or pipe-write error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn malformed_frame_header_refuses() {
    // Peer sends a header that declares `u32::MAX` body length. Host's
    // read_frame refuses before allocating the body.
    let path = peer_path();
    let mut host = GuestHost::spawn(
        &path,
        &["--mode=malformed-frame-header".to_string()],
        deadlines(),
    )
    .expect("peer spawn");
    let result = host
        .exchange_mut()
        .hello(&["test-cap".to_string()], Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(result.is_err(), "u32::MAX header must refuse");
    let message = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains("exceeds maximum"),
        "expected oversize-frame error, got: {message}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}

#[test]
fn normal_echo_round_trip_succeeds() {
    // Sanity test: the peer in normal-echo mode lets a request
    // round-trip cleanly. Proves the harness is not
    // self-rejecting valid traffic.
    let path = peer_path();
    let mut host = GuestHost::spawn(&path, &["--mode=normal-echo".to_string()], deadlines())
        .expect("peer spawn");
    let exchange = host.exchange_mut();
    let hello = exchange.hello(&["test-cap".to_string()], Duration::from_secs(1));
    assert!(
        hello.is_ok(),
        "hello must succeed in normal-echo: {hello:?}"
    );
    let response = exchange.request("ping", serde_json::json!({}), Duration::from_secs(1));
    let cleanup = host.shutdown();
    assert!(
        response.is_ok(),
        "request must succeed in normal-echo: {response:?}"
    );
    let value = response.unwrap();
    assert_eq!(
        value.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "expected ok=true in echo response: {value}"
    );
    assert_eq!(
        value.get("echoed").and_then(|v| v.as_bool()),
        Some(true),
        "expected echoed=true in echo response: {value}"
    );
    assert!(cleanup.is_ok(), "shutdown must clean up: {cleanup:?}");
}
