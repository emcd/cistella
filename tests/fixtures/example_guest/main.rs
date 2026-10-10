//! Minimal correct-behavior example guest: the builders' reference
//! for third-party extensions.
//!
//! Unlike the deterministic peer (`fake_guest`, a fault injector),
//! this guest implements zero lifecycle meaning correctly: hello
//! advertising one role string plus a prepare acknowledgement with
//! empty contributions (environment, mounts, policy claims, hooks).
//! A registered extension answering this way converges on the plain
//! path — hooks were never promised.
//!
//! Behavior is selected by argv (the protocol host spawns a pinned
//! absolute path, so behavior stays scriptable through argv):
//!
//!   `--role=<name>`   hello role string (default `example`)
//!
//! Cargo treats this file as an example target only (see
//! `Cargo.toml`'s `[[example]]` tables); `package.include`
//! excludes `tests/**`, so the guest never ships to crates.io.

use std::io::{Read, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

const PROTOCOL_MAJOR: u32 = 1;
const HEADER_LEN: usize = 4;

fn read_role() -> String {
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--role=") {
            return value.to_string();
        }
    }
    "example".to_string()
}

fn read_frame<R: Read>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> std::io::Result<()> {
    const MAX_FRAME: usize = 64 * 1024;
    if payload.len() > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("frame length {} exceeds {MAX_FRAME}", payload.len()),
        ));
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

fn main() -> ExitCode {
    let role = read_role();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    // Hello: consume the host's hello, advertise the role only.
    if read_frame(&mut stdin_lock).is_err() {
        return ExitCode::from(2);
    }
    let hello = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": "hello",
        "op": "hello",
        "payload": {
            "version": PROTOCOL_MAJOR,
            "capabilities": [role],
            "max_frame": 1024u32
        }
    });
    let body = serde_json::to_vec(&hello).expect("hello serializes");
    if write_frame(&mut stdout_lock, &body).is_err() {
        return ExitCode::from(2);
    }

    // Prepare: echo the request id back with empty contributions.
    let request = match read_frame(&mut stdin_lock) {
        Ok(body) => body,
        Err(_) => return ExitCode::from(2),
    };
    let id: Value = serde_json::from_slice(&request)
        .ok()
        .and_then(|envelope: Value| envelope.get("id").cloned())
        .unwrap_or(json!("req-0"));
    let response = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": id,
        "op": "prepare",
        "payload": {
            "environment": [],
            "mounts": [],
            "policy_claims": [],
            "guest_hooks": []
        }
    });
    let body = serde_json::to_vec(&response).expect("prepare serializes");
    if write_frame(&mut stdout_lock, &body).is_err() {
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
