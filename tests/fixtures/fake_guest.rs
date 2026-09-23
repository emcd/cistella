//! Deterministic protocol peer for conformance harness fault injection.
//!
//! Spawned by `framework::protocol::GuestHost` exactly like a real
//! extension/isolator helper: a pinned executable that produces
//! scripted fault shapes at the stdio boundary. Implements zero
//! lifecycle meaning — never satisfies a lifecycle-common test
//! (task 3.1 applicability rule #1). The peer proves that the host
//! refuses or recovers deterministically; the host is what the suite
//! tests, not the peer.
//!
//! Behavior is selected by argv flags (the protocol host calls
//! `GuestHost::spawn(absolute_path, args, deadlines)`, so the path is
//! already pinned and behavior stays scriptable through argv):
//!
//!   `--mode=hello-version-mismatch`     hello with wrong `version`
//!   `--mode=hello-bad-capability`       hello with unknown capability name
//!   `--mode=hello-then-eof`             hello, then close stdin
//!   `--mode=malformed-frame-header`     send a header that exceeds negotiated max
//!   `--mode=oversize-frame`             declare length far above `PRE_NEGOTIATION_MAX_FRAME`
//!   `--mode=unknown-fields`             envelope with `deny_unknown_fields` violation
//!   `--mode=duplicate-id`               echo two responses with the same id
//!   `--mode=pending-on-solo`            send `{pending: true}` on a single-shot exchange
//!   `--mode=hang-hello`                 sleep forever before hello
//!   `--mode=hang-request`               hello, then sleep forever
//!   `--mode=partial-response`           declare body length, deliver less
//!   `--mode=spurious-after-terminal`    hello + valid response, then send a stray frame
//!   `--mode=cleanup-then-write`         hello, kill own stdin, try to write to stdout
//!   `--mode=stderr-fill`                write to stderr past `STDERR_CAP` then exit
//!   `--mode=normal-echo`                hello + echo requests one-for-one (default)
//!
//! Mode defaults to `normal-echo` for unknown values, which itself
//! proves the host treats the peer as untrusted input and refuses any
//! unexpected shape.
//!
//! Cargo treats this file as a test target only (see `Cargo.toml`'s
//! `[[test]]` block); `package.include` excludes `tests/**`, so the
//! peer never ships to crates.io.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

const PROTOCOL_MAJOR: u32 = 1;

fn read_arg_mode() -> String {
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--mode=") {
            return value.to_string();
        }
    }
    "normal-echo".to_string()
}

fn protocol_error_exit() -> ExitCode {
    ExitCode::from(2)
}

/// Minimal length-prefixed frame writer that respects the pre-negotiation
/// ceiling. Mirrors `framework::protocol::write_frame`'s shape so the
/// host's framing code is exercised symmetrically.
fn write_frame<W: Write>(stdout: &mut W, payload: &[u8], max_frame: usize) -> std::io::Result<()> {
    if payload.len() > max_frame {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("frame length {} exceeds {max_frame}", payload.len()),
        ));
    }
    let header = (payload.len() as u32).to_be_bytes();
    stdout.write_all(&header)?;
    stdout.write_all(payload)?;
    stdout.flush()
}

fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    reader.read_exact(buf)
}

/// Reads one length-prefixed frame from stdin (header + body). Returns
/// the body bytes when the host's wire shape is correct, or `Err` when
/// the pipe is shorter than the declared length (host closed early or
/// wire shape is malformed). Used by fault modes that need to keep
/// stdin byte-aligned with the host's send/recv cadence.
fn read_frame_from_stdin<R: Read>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; HEADER_LEN];
    read_exact(reader, &mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    let mut body = vec![0u8; len];
    read_exact(reader, &mut body)?;
    Ok(body)
}

fn hello_response_ok() -> Value {
    json!({
        "protocol": PROTOCOL_MAJOR,
        "id": "hello",
        "op": "hello",
        "payload": {
            "version": PROTOCOL_MAJOR,
            "capabilities": ["test-cap"],
            "max_frame": 1024u32
        }
    })
}

fn hello_response_version_mismatch() -> Value {
    json!({
        "protocol": PROTOCOL_MAJOR + 99,
        "id": "hello",
        "op": "hello",
        "payload": {
            "version": PROTOCOL_MAJOR + 99,
            "capabilities": ["test-cap"]
        }
    })
}
fn hello_response_unknown_field() -> ExitCode {
    // Field `smuggled` is rejected by `deny_unknown_fields` on the
    // host's Envelope schema. The host's parse_envelope refuses
    // before any protocol step runs; we send valid JSON that the
    // schema refuses.
    //
    // Write directly to fd 1 (kernel pipe, unbuffered) instead of
    // through `std::io::Stdout` (Rust's line-buffered wrapper that
    // races with process exit). The 50ms pre-exit sleep is belt and
    // braces against any remaining kernel-drain timing surface.
    let raw = br#"{"protocol":1,"id":"hello","op":"hello","payload":{"version":1,"capabilities":["test-cap"]},"smuggled":true}"#;
    let mut header = [0u8; 4];
    let len = raw.len() as u32;
    header.copy_from_slice(&len.to_be_bytes());
    let header_write = unsafe { libc::write(1, &header as *const u8 as *const _, HEADER_LEN) };
    let body_write = unsafe { libc::write(1, raw.as_ptr() as *const _, raw.len()) };
    debug_assert_eq!(header_write, HEADER_LEN as isize);
    debug_assert_eq!(body_write, raw.len() as isize);
    std::thread::sleep(Duration::from_millis(50));
    ExitCode::SUCCESS
}

const HEADER_LEN: usize = 4;

mod libc {
    use std::ffi::c_void;
    unsafe extern "C" {
        pub unsafe fn write(fd: i32, buf: *const c_void, count: usize) -> isize;
        pub unsafe fn close(fd: i32) -> i32;
    }
}
fn main() -> ExitCode {
    let mode = read_arg_mode();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    match mode.as_str() {
        "hello-version-mismatch" => {
            // Read the host hello frame (raw bytes), then reply with mismatched version.
            let mut header = [0u8; 4];
            if read_exact(&mut stdin_lock, &mut header).is_err() {
                return protocol_error_exit();
            }
            let response = hello_response_version_mismatch();
            let body = serde_json::to_vec(&response).expect("serialize");
            if write_frame(&mut stdout_lock, &body, 64 * 1024).is_err() {
                return protocol_error_exit();
            }
            ExitCode::SUCCESS
        }
        "hello-bad-capability" => {
            let mut header = [0u8; 4];
            if read_exact(&mut stdin_lock, &mut header).is_err() {
                return protocol_error_exit();
            }
            let body = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["not-a-real-capability-with-uuid-aaaa"],
                }
            }))
            .expect("serialize");
            if write_frame(&mut stdout_lock, &body, 64 * 1024).is_err() {
                return protocol_error_exit();
            }
            ExitCode::SUCCESS
        }
        "hello-then-eof" => {
            let mut header = [0u8; 4];
            let _ = read_exact(&mut stdin_lock, &mut header);
            // Reply with a valid hello then close our stdout (host sees EOF on next recv).
            let body = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body, 64 * 1024);
            // Drop stdout to close it.
            drop(stdout_lock);
            ExitCode::SUCCESS
        }
        "malformed-frame-header" => {
            let mut header = [0u8; 4];
            let _ = read_exact(&mut stdin_lock, &mut header);
            // Send a header that claims a body way over the pre-negotiation ceiling.
            let bad_len: u32 = u32::MAX;
            let header_bytes = bad_len.to_be_bytes();
            let _ = stdout_lock.write_all(&header_bytes);
            let _ = stdout_lock.flush();
            ExitCode::SUCCESS
        }
        "oversize-frame" => {
            let mut header = [0u8; 4];
            let _ = read_exact(&mut stdin_lock, &mut header);
            // Same shape as `malformed-frame-header` but with a length just over
            // `PRE_NEGOTIATION_MAX_FRAME` (64 KiB) — explicit oversize rejection path.
            let bad_len: u32 = 65 * 1024;
            let header_bytes = bad_len.to_be_bytes();
            let _ = stdout_lock.write_all(&header_bytes);
            let _ = stdout_lock.flush();
            ExitCode::SUCCESS
        }
        "unknown-fields" => hello_response_unknown_field(),
        "duplicate-id" => {
            let mut header = [0u8; 4];
            let _ = read_exact(&mut stdin_lock, &mut header);
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body1, 64 * 1024);
            // Now send a second response with the same id but a different op —
            // the host's correlation check should refuse the duplicate id.
            let body2 = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {"version": PROTOCOL_MAJOR, "capabilities": ["test-cap"]}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body2, 64 * 1024);
            ExitCode::SUCCESS
        }
        "pending-on-solo" => {
            let _ = read_frame_from_stdin(&mut stdin_lock);
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body1, 64 * 1024);
            // Read the host's first request frame...
            let _ = read_frame_from_stdin(&mut stdin_lock);
            // ...then reply with `{pending: true}` on what should be a solo exchange.
            let body2 = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "req-0",
                "op": "ping",
                "payload": {"pending": true}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body2, 64 * 1024);
            ExitCode::SUCCESS
        }
        "hang-hello" => {
            // Read the host hello (so the host does not get EOF on its first recv),
            // then sleep forever. The host's hello deadline (5s default) fires.
            let mut header = [0u8; 4];
            let _ = read_exact(&mut stdin_lock, &mut header);
            std::thread::sleep(Duration::from_secs(60));
            ExitCode::SUCCESS
        }
        "hang-request" => {
            let _ = read_frame_from_stdin(&mut stdin_lock);
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body1, 64 * 1024);
            // After hello, sleep before responding to any request.
            std::thread::sleep(Duration::from_secs(60));
            ExitCode::SUCCESS
        }
        "partial-response" => {
            let mut header = [0u8; 4];
            let _ = read_exact(&mut stdin_lock, &mut header);
            // Declare body length 1024 bytes, then deliver only 8.
            let declared_len: u32 = 1024;
            let header_bytes = declared_len.to_be_bytes();
            let _ = stdout_lock.write_all(&header_bytes);
            let _ = stdout_lock.write_all(b"partial!");
            let _ = stdout_lock.flush();
            // Close stdout to surface EOF mid-frame.
            drop(stdout_lock);
            ExitCode::SUCCESS
        }
        "spurious-after-terminal" => {
            let _ = read_frame_from_stdin(&mut stdin_lock);
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body1, 64 * 1024);
            // Read the host's request frame...
            let _ = read_frame_from_stdin(&mut stdin_lock);
            // ...reply validly...
            let body2 = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "req-0",
                "op": "ping",
                "payload": {"ok": true}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body2, 64 * 1024);
            // ...then send a stray frame after the terminal response.
            let stray = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "stray",
                "op": "ghost",
                "payload": {"ok": true}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &stray, 64 * 1024);
            ExitCode::SUCCESS
        }
        "cleanup-then-write" => {
            let _ = read_frame_from_stdin(&mut stdin_lock);
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body1, 64 * 1024);
            // Read the host's request, then close fd 1 entirely so any
            // subsequent write fails with EBADF. The host's recv sees
            // EOF on the pipe and surfaces a typed Protocol error.
            let _ = read_frame_from_stdin(&mut stdin_lock);
            drop(stdout_lock);
            // SAFETY: closing fd 1 (stdout) is intentional fault
            // injection. After this, any write to fd 1 returns EBADF.
            // The peer's main returns immediately and the process
            // exits, so fd 1 stays closed for the remainder of the
            // pipe lifetime. Bounded to this arm.
            let close_result = unsafe { libc::close(1) };
            eprintln!("DEBUG: libc::close(1) returned {close_result}");
            std::thread::sleep(Duration::from_millis(100));
            ExitCode::SUCCESS
        }
        "stderr-fill" => {
            // Never write to stdout (host hangs on hello until deadline).
            // Fill stderr past STDERR_CAP (1 MiB).
            let stderr = std::io::stderr();
            let mut handle = stderr.lock();
            let chunk = vec![b'x'; 8192];
            for _ in 0..200 {
                let _ = handle.write_all(&chunk);
            }
            let _ = handle.flush();
            ExitCode::SUCCESS
        }
        "normal-echo" => {
            // Default: hello + echo one request frame as a response.
            if read_frame_from_stdin(&mut stdin_lock).is_err() {
                return protocol_error_exit();
            }
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            if write_frame(&mut stdout_lock, &body1, 64 * 1024).is_err() {
                return protocol_error_exit();
            }
            // Echo the next request frame back as a valid response.
            let req_body = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return protocol_error_exit(),
            };
            // Parse and respond with the same id/op.
            if let Ok(envelope) = serde_json::from_slice::<Value>(&req_body) {
                let id = envelope.get("id").cloned().unwrap_or(json!("req-0"));
                let op = envelope.get("op").cloned().unwrap_or(json!("echo"));
                let response = json!({
                    "protocol": PROTOCOL_MAJOR,
                    "id": id,
                    "op": op,
                    "payload": {"ok": true, "echoed": true}
                });
                let body = serde_json::to_vec(&response).expect("serialize");
                let _ = write_frame(&mut stdout_lock, &body, 64 * 1024);
            }
            ExitCode::SUCCESS
        }
        _ => {
            // Unknown mode falls back to `normal-echo` behavior —
            // the host treats the peer as untrusted input and any
            // deviation from the negotiated protocol must surface
            // as a typed error rather than a silent success.
            let mut header = [0u8; 4];
            if read_exact(&mut stdin_lock, &mut header).is_err() {
                return protocol_error_exit();
            }
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body1, 64 * 1024);
            ExitCode::SUCCESS
        }
    }
}
