//! Framing/timeout fault modes for the deterministic protocol peer.
//!
//! Grouped from the single-file peer at the file-size limit; wire
//! shapes unchanged. See `main.rs` for shared framing helpers.

use std::io::Write;
use std::io::{StdinLock, StdoutLock};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    HEADER_LEN, PROTOCOL_MAJOR, hello_response_ok, libc, protocol_error_exit, read_exact,
    read_frame_from_stdin, write_frame,
};

/// Runs one Framing/timeout mode; `None` falls through to the next group.
pub(crate) fn run(
    mode: &str,
    mut stdin_lock: &mut StdinLock<'_>,
    mut stdout_lock: &mut StdoutLock<'_>,
) -> Option<ExitCode> {
    Some(match mode {
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
            // SAFETY: closing fd 1 (stdout) is intentional fault
            // injection. After this, any write to fd 1 returns EBADF.
            // The peer's main returns immediately and the process
            // exits, so fd 1 stays closed for the remainder of the
            // pipe lifetime. Bounded to this arm.
            unsafe {
                libc::close(1);
            }
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
                return Some(protocol_error_exit());
            }
            let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            if write_frame(&mut stdout_lock, &body1, 64 * 1024).is_err() {
                return Some(protocol_error_exit());
            }
            // Echo the next request frame back as a valid response.
            let req_body = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
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
        // ---- Prepare-transaction fault modes (task 3.1, peer-driven
        //      boundary coverage per the frozen 2.2 wire shape).
        //
        // All prepare-fault modes share the same shape:
        //   1. Read hello + write hello response.
        //   2. Read prepare request (consume both header and body).
        //   3. Write a prepare response carrying the fault payload.
        //
        // The host's `run_prepare` then exercises its merge/lattice
        // machinery; the test asserts the typed Contract refusal.
        _ => return None,
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
