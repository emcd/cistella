//! Hello-negotiation fault modes for the deterministic protocol peer.
//!
//! Grouped from the single-file peer at the file-size limit; wire
//! shapes unchanged. See `main.rs` for shared framing helpers.

use std::io::{StdinLock, StdoutLock};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    PROTOCOL_MAJOR, hello_response_ok, protocol_error_exit, read_exact, read_frame_from_stdin,
    write_frame,
};

/// Runs one Hello-negotiation mode; `None` falls through to the next group.
pub(crate) fn run(
    mode: &str,
    mut stdin_lock: &mut StdinLock<'_>,
    mut stdout_lock: &mut StdoutLock<'_>,
) -> Option<ExitCode> {
    Some(match mode {
        "hello-version-mismatch" => {
            // Read the host hello frame (raw bytes), then reply with mismatched version.
            let mut header = [0u8; 4];
            if read_exact(&mut stdin_lock, &mut header).is_err() {
                return Some(protocol_error_exit());
            }
            let response = hello_response_version_mismatch();
            let body = serde_json::to_vec(&response).expect("serialize");
            if write_frame(&mut stdout_lock, &body, 64 * 1024).is_err() {
                return Some(protocol_error_exit());
            }
            ExitCode::SUCCESS
        }
        "hello-bad-capability" => {
            let mut header = [0u8; 4];
            if read_exact(&mut stdin_lock, &mut header).is_err() {
                return Some(protocol_error_exit());
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
                return Some(protocol_error_exit());
            }
            ExitCode::SUCCESS
        }
        "hello-real-capabilities" => {
            // Advertises the five real framework capability names so
            // the production closed-negotiation path (`host_external`)
            // accepts the set; the harness still treats every
            // subsequent shape as untrusted input.
            let mut header = [0u8; 4];
            if read_exact(&mut stdin_lock, &mut header).is_err() {
                return Some(protocol_error_exit());
            }
            let body = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": [
                        "environment",
                        "mounts",
                        "policy-claims",
                        "guest-hooks",
                        "credentials"
                    ],
                }
            }))
            .expect("serialize");
            if write_frame(&mut stdout_lock, &body, 64 * 1024).is_err() {
                return Some(protocol_error_exit());
            }
            // Serve one request like `normal-echo` so the host can
            // proceed past hello if it wishes, then exit.
            let req_body = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
            };
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
        "hello-evil-capability" => {
            // Advertises a capability name carrying control bytes and
            // a fake secret assignment. Refusal diagnostics must
            // never render these bytes: the host identifies the
            // offender by index only. Kept terse (no request service)
            // so the host's shutdown path owns the exit.
            let mut header = [0u8; 4];
            if read_exact(&mut stdin_lock, &mut header).is_err() {
                return Some(protocol_error_exit());
            }
            let body = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["BAD\nCAP\x01SECRET=evil-sentinel-value"],
                }
            }))
            .expect("serialize");
            if write_frame(&mut stdout_lock, &body, 64 * 1024).is_err() {
                return Some(protocol_error_exit());
            }
            std::thread::sleep(Duration::from_secs(60));
            ExitCode::SUCCESS
        }
        "hello-then-eof" => {
            let mut header = [0u8; 4];
            let _ = read_exact(&mut stdin_lock, &mut header);
            // Reply with a valid hello then close our stdout (host sees EOF on next recv).
            let body = serde_json::to_vec(&hello_response_ok()).expect("serialize");
            let _ = write_frame(&mut stdout_lock, &body, 64 * 1024);
            // Drop stdout to close it.
            ExitCode::SUCCESS
        }
        _ => return None,
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
