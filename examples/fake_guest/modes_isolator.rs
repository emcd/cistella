//! Isolator-concurrency fault modes for the deterministic protocol peer.
//!
//! Grouped from the single-file peer at the file-size limit; wire
//! shapes unchanged. See `main.rs` for shared framing helpers.

use std::io::Write;
use std::io::{StdinLock, StdoutLock};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{PROTOCOL_MAJOR, protocol_error_exit, read_frame_from_stdin, write_frame};

/// Runs one Isolator-concurrency mode; `None` falls through to the next group.
pub(crate) fn run(
    mode: &str,
    mut stdin_lock: &mut StdinLock<'_>,
    mut stdout_lock: &mut StdoutLock<'_>,
) -> Option<ExitCode> {
    Some(match mode {
        "isolator-concurrent" => {
            // Scripted isolator ops for client-dispatcher proofs:
            // hello with the isolator role cap, then a slow await
            // op followed by a second op that MUST arrive while the
            // await pends (the client must send it concurrently,
            // not after). Replies go out in completion order: the
            // second op first, the await terminal second.
            if read_frame_from_stdin(&mut stdin_lock).is_err() {
                return Some(protocol_error_exit());
            }
            let hello = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["isolator"],
                    "max_frame": 1024u32
                }
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &hello, 64 * 1024);
            let op1 = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
            };
            let op1_id = serde_json::from_slice::<Value>(&op1)
                .ok()
                .and_then(|env| env.get("id").cloned())
                .unwrap_or(json!("await-0"));
            std::thread::sleep(Duration::from_secs(3));
            let op2 = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
            };
            let op2_id = serde_json::from_slice::<Value>(&op2)
                .ok()
                .and_then(|env| env.get("id").cloned())
                .unwrap_or(json!("op2-0"));
            let reply2 = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": op2_id,
                "op": "isolator.terminate",
                "payload": {"ok": {"stopped_attestation": {"unit_identity": "concurrent01"}}}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &reply2, 64 * 1024);
            // Two heartbeat frames before the terminal: the client
            // must keep the await entry (counting, forwarding
            // nothing) and redeem only on the terminal.
            for _ in 0..2 {
                let pending = serde_json::to_vec(&json!({
                    "protocol": PROTOCOL_MAJOR,
                    "id": op1_id,
                    "op": "isolator.await_result",
                    "payload": {"pending": true}
                }))
                .expect("serialize");
                let _ = write_frame(&mut stdout_lock, &pending, 64 * 1024);
                std::thread::sleep(Duration::from_millis(200));
            }
            let reply1 = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": op1_id,
                "op": "isolator.await_result",
                "payload": {"ok": {"exit_status": 0}}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &reply1, 64 * 1024);
            ExitCode::SUCCESS
        }
        "isolator-die-mid-await" => {
            // Hello, then die abruptly after the first op arrives:
            // the client must fail every pending caller with a
            // typed error (never hang) instead of treating death
            // as silence.
            if read_frame_from_stdin(&mut stdin_lock).is_err() {
                return Some(protocol_error_exit());
            }
            let hello = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["isolator"],
                    "max_frame": 1024u32
                }
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &hello, 64 * 1024);
            let _ = read_frame_from_stdin(&mut stdin_lock);
            std::process::exit(1);
        }
        "isolator-split-frame" => {
            // Hello, then answer one op with the frame split
            // across dispatch slices: 4-byte header, sleep past
            // two slices, then the body. The dispatcher's
            // persistent assembler must resolve it (a
            // fresh-assembler-per-slice design corrupts here).
            if read_frame_from_stdin(&mut stdin_lock).is_err() {
                return Some(protocol_error_exit());
            }
            let hello = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["isolator"],
                    "max_frame": 1024u32
                }
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &hello, 64 * 1024);
            let op_body = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
            };
            let id = serde_json::from_slice::<Value>(&op_body)
                .ok()
                .and_then(|env| env.get("id").cloned())
                .unwrap_or(json!("split-0"));
            let response = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": id,
                "op": "isolator.state",
                "payload": {"ok": {"lifecycle": "initiated"}}
            }))
            .expect("serialize");
            let header = (response.len() as u32).to_be_bytes();
            // Length header first, direct (write_frame always
            // writes whole frames); body after the sleep.
            {
                let _ = stdout_lock.write_all(&header);
                let _ = stdout_lock.flush();
            }
            std::thread::sleep(Duration::from_millis(300));
            {
                let _ = stdout_lock.write_all(&response);
                let _ = stdout_lock.flush();
            }
            ExitCode::SUCCESS
        }
        "isolator-wrong-op" => {
            // Hello, then answer the op id with a DIFFERENT op
            // name: the dispatcher must refuse the mismatch
            // instead of delivering it to the caller.
            if read_frame_from_stdin(&mut stdin_lock).is_err() {
                return Some(protocol_error_exit());
            }
            let hello = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["isolator"],
                    "max_frame": 1024u32
                }
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &hello, 64 * 1024);
            let op_body = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
            };
            let id = serde_json::from_slice::<Value>(&op_body)
                .ok()
                .and_then(|env| env.get("id").cloned())
                .unwrap_or(json!("wrong-0"));
            let response = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": id,
                "op": "isolator.evil",
                "payload": {"ok": {"lifecycle": "initiated"}}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &response, 64 * 1024);
            ExitCode::SUCCESS
        }
        "isolator-unsolicited" => {
            // Hello, then emit a response for an id nobody asked
            // about: the dispatcher must fail the exchange (fail
            // all pending, shut down) rather than drop it
            // silently. A valid reply follows, but the caller
            // must already have failed.
            if read_frame_from_stdin(&mut stdin_lock).is_err() {
                return Some(protocol_error_exit());
            }
            let hello = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["isolator"],
                    "max_frame": 1024u32
                }
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &hello, 64 * 1024);
            let op_body = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
            };
            let id = serde_json::from_slice::<Value>(&op_body)
                .ok()
                .and_then(|env| env.get("id").cloned())
                .unwrap_or(json!("real-0"));
            let ghost = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "ghost-nobody-asked",
                "op": "isolator.state",
                "payload": {"ok": {"lifecycle": "initiated"}}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &ghost, 64 * 1024);
            std::thread::sleep(Duration::from_millis(200));
            let real = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": id,
                "op": "isolator.state",
                "payload": {"ok": {"lifecycle": "initiated"}}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &real, 64 * 1024);
            ExitCode::SUCCESS
        }
        "isolator-big-frame" => {
            // Hello with an 8 MiB ceiling, then answer one op with
            // a 2 MiB payload: between the 1 MiB default and the
            // negotiated ceiling. The dispatcher must route it as
            // a normal response (its reader uses the negotiated
            // bound, not the default).
            if read_frame_from_stdin(&mut stdin_lock).is_err() {
                return Some(protocol_error_exit());
            }
            let hello = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": "hello",
                "op": "hello",
                "payload": {
                    "version": PROTOCOL_MAJOR,
                    "capabilities": ["isolator"],
                    "max_frame": 8388608u32
                }
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &hello, 64 * 1024);
            let op_body = match read_frame_from_stdin(&mut stdin_lock) {
                Ok(body) => body,
                Err(_) => return Some(protocol_error_exit()),
            };
            let id = serde_json::from_slice::<Value>(&op_body)
                .ok()
                .and_then(|env| env.get("id").cloned())
                .unwrap_or(json!("big-0"));
            let pad = "p".repeat(2 * 1024 * 1024);
            let response = serde_json::to_vec(&json!({
                "protocol": PROTOCOL_MAJOR,
                "id": id,
                "op": "isolator.state",
                "payload": {"ok": {"lifecycle": "initiated", "pad": pad}}
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &response, 8 * 1024 * 1024);
            ExitCode::SUCCESS
        }
        _ => return None,
    })
}
