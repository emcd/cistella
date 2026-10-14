//! SSH extension: seat agent-socket forwarding as a guest
//! prepare contribution. Hello advertises its role plus the
//! contribution types it returns, then one prepare transaction
//! contributes the `SSH_AUTH_SOCK` pointer env (declared path),
//! the socket bind read-only (canonical source onto the declared
//! target), a `read` permission on the declared target, and the
//! `seat-socket` credential handle for the canonical path. A
//! zero-hook answer converges plain downstream.
//!
//! Behavior is selected by argv (the protocol host spawns a pinned
//! absolute path, so behavior stays scriptable through argv):
//!
//!   `--socket=<P>`            declared guest-visible socket path
//!                             (env value, container target,
//!                             permission target)
//!   `--socket-canonical=<C>`  canonical host-authority path
//!                             (mount source, credential locator)
//!
//! Both forms are host-supplied; the guest never probes the
//! filesystem for identity. Its only filesystem read is the
//! `S_ISSOCK` kind check on the canonical path before
//! contributing (sockets cannot be `read()`-probed; a missing or
//! non-socket object fails the prepare loudly, never an
//! undeclared session). Hello and prepare frames gate on epoch
//! and identity before any answer (a legacy host gets refusal
//! and silence, never a contribution).
//!
//! Shipped sibling binary (autobins): installed and discovered
//! beside the driver like `cistella-extension-tmpdir`.

use std::io::{Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::process::ExitCode;

use serde_json::{Value, json};

use cistella::framework::protocol::PROTOCOL_MAJOR;

const HEADER_LEN: usize = 4;

fn read_argv() -> (String, Option<String>, Option<String>) {
    let mut role = "ssh".to_string();
    let mut socket = None;
    let mut canonical = None;
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--role=") {
            role = value.to_string();
        } else if let Some(value) = arg.strip_prefix("--socket=") {
            socket = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--socket-canonical=") {
            canonical = Some(value.to_string());
        }
    }
    (role, socket, canonical)
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

/// The canonical path must be a socket: following metadata must
/// report `S_ISSOCK`. Missing paths and non-socket objects fail;
/// nothing is opened or read (sockets do not work that way).
fn check_socket(canonical: &str) -> bool {
    std::fs::metadata(canonical)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

fn main() -> ExitCode {
    let (role, socket, canonical) = read_argv();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    // Hello: validate the host hello (epoch and op) BEFORE
    // answering. A legacy or malformed hello refuses here with
    // nothing emitted — version-first refusal, never planning
    // on a mismatched wire.
    let hello_body = match read_frame(&mut stdin_lock) {
        Ok(body) => body,
        Err(_) => return ExitCode::from(2),
    };
    let hello_request: Value = serde_json::from_slice(&hello_body).unwrap_or(json!({}));
    if hello_request
        .get("protocol")
        .and_then(|value| value.as_u64())
        != Some(PROTOCOL_MAJOR as u64)
        || hello_request.get("op").and_then(|value| value.as_str()) != Some("hello")
    {
        return ExitCode::from(2);
    }
    let hello = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": "hello",
        "op": "hello",
        "payload": {
            "version": PROTOCOL_MAJOR,
            "capabilities": [role, "environment", "mounts", "permissions", "credentials"],
            // Honest bound: the host enforces the advertised
            // maximum (clamped to its ceiling), so it must cover
            // the largest serializable tuple — the write path
            // below refuses past this same bound.
            "max_frame": 65536u32
        }
    });
    let body = serde_json::to_vec(&hello).expect("hello serializes");
    if write_frame(&mut stdout_lock, &body).is_err() {
        return ExitCode::from(2);
    }

    // Prepare: gate epoch, op, and session identity BEFORE any
    // filesystem contact; gate argv and socket kind before any
    // answer. Any failure refuses with silence after hello —
    // never a partial contribution.
    let request = match read_frame(&mut stdin_lock) {
        Ok(body) => body,
        Err(_) => return ExitCode::from(2),
    };
    let request: Value = serde_json::from_slice(&request).unwrap_or(json!({}));
    if request.get("protocol").and_then(|value| value.as_u64()) != Some(PROTOCOL_MAJOR as u64)
        || request.get("op").and_then(|value| value.as_str()) != Some("prepare")
    {
        return ExitCode::from(2);
    }
    let id = request.get("id").cloned().unwrap_or(json!("req-0"));
    let session_ok = request
        .get("payload")
        .and_then(|payload| payload.get("session_id"))
        .and_then(|value| value.as_str())
        .is_some();
    if !session_ok {
        return ExitCode::from(2);
    }
    let (declared, canonical) = match (socket, canonical) {
        (Some(declared), Some(canonical)) => (declared, canonical),
        _ => return ExitCode::from(2),
    };
    if !check_socket(&canonical) {
        return ExitCode::from(2);
    }
    // Exact tuple only: env names the declared path, the mount
    // binds the canonical source onto the declared target
    // read-only, the permission names the declared (guest-visible)
    // target, and the credential handle names the canonical
    // (host-authority) path. No claims, no hooks, no cleanup.
    let response = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": id,
        "op": "prepare",
        "payload": {
            "environment": [
                {
                    "name": "SSH_AUTH_SOCK",
                    "value": declared
                }
            ],
            "mounts": [
                {
                    "host_source": canonical,
                    "container_target": declared
                }
            ],
            "permissions": [
                {
                    "target": declared,
                    "rights": "read"
                }
            ],
            "policy_claims": [],
            "guest_hooks": [],
            "credentials": [
                {
                    "kind": "seat-socket",
                    "path": canonical
                }
            ]
        }
    });
    let body = serde_json::to_vec(&response).expect("prepare serializes");
    if write_frame(&mut stdout_lock, &body).is_err() {
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
