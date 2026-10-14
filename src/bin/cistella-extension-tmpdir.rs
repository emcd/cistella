//! Tmpdir extension: per-session writable `/tmp` backed by a
//! host slice. Hello advertises its role plus the contribution
//! types it returns, then one prepare transaction mints an
//! owner-only uniquely-named slice, contributes it as a
//! read-write triple targeting `/tmp`, and declares it for
//! cleanup. A zero-hook answer converges plain downstream.
//!
//! Behavior is selected by argv (the protocol host spawns a pinned
//! absolute path, so behavior stays scriptable through argv):
//!
//!   `--role=<name>`   hello role string (default `tmpdir`)
//!
//! Slice names embed the prepare session id for attribution
//! (`cistella-tmp-<session>-<pid>-<nanos>`, O_EXCL create, 0700).
//! Mint failure fails the prepare loudly (fail-closed), never an
//! undeclared session. Hello and prepare frames gate on epoch
//! and identity before any answer or mint (a legacy host gets
//! refusal and silence, never a slice).
//!
//! Shipped sibling binary (autobins): installed and discovered
//! beside the driver like `cistella-extension-landlock`.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::ExitCode;

use serde_json::{Value, json};

use cistella::framework::protocol::PROTOCOL_MAJOR;

const HEADER_LEN: usize = 4;

fn read_role() -> String {
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--role=") {
            return value.to_string();
        }
    }
    "tmpdir".to_string()
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

/// Mints the session slice under host `/tmp`: fixed prefix plus
/// session id plus process id plus nanos, exclusive create,
/// owner-only mode. Any failure fails loudly (the prepare must
/// refuse, never run undeclared). The parent is always host
/// `/tmp` — no environment selection, no knobs.
fn mint_slice(session: &str) -> std::io::Result<std::path::PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let name = format!("cistella-tmp-{session}-{}-{nanos}", std::process::id());
    let path = std::path::PathBuf::from("/tmp").join(name);
    std::fs::create_dir(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn main() -> ExitCode {
    let role = read_role();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    // Hello: validate the host hello (epoch and op) BEFORE
    // answering. A legacy or malformed hello refuses here with
    // nothing emitted — version-first refusal, never minting or
    // planning on a mismatched wire.
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
            "capabilities": [role, "mounts", "permissions", "cleanup"],
            "max_frame": 1024u32
        }
    });
    let body = serde_json::to_vec(&hello).expect("hello serializes");
    if write_frame(&mut stdout_lock, &body).is_err() {
        return ExitCode::from(2);
    }

    // Prepare: mint the slice keyed by the request's session id,
    // then answer the triple plus the cleanup declaration.
    let request = match read_frame(&mut stdin_lock) {
        Ok(body) => body,
        Err(_) => return ExitCode::from(2),
    };
    let request: Value = serde_json::from_slice(&request).unwrap_or(json!({}));
    // Prepare epoch and identity gate BEFORE minting: the
    // envelope must speak this epoch with the prepare op, and
    // the opaque session id must be present (minting keyed by a
    // fallback identity would break attribution).
    if request.get("protocol").and_then(|value| value.as_u64()) != Some(PROTOCOL_MAJOR as u64)
        || request.get("op").and_then(|value| value.as_str()) != Some("prepare")
    {
        return ExitCode::from(2);
    }
    let id = request.get("id").cloned().unwrap_or(json!("req-0"));
    let session = match request
        .get("payload")
        .and_then(|payload| payload.get("session_id"))
        .and_then(|value| value.as_str())
    {
        Some(session) => session,
        None => return ExitCode::from(2),
    };
    let slice = match mint_slice(session) {
        Ok(path) => path,
        Err(_) => return ExitCode::from(2),
    };
    let response = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": id,
        "op": "prepare",
        "payload": {
            "environment": [],
            "mounts": [
                {
                    "host_source": slice.to_string_lossy(),
                    "container_target": "/tmp"
                }
            ],
            "permissions": [
                {
                    "target": "/tmp",
                    "rights": "full"
                }
            ],
            "policy_claims": [],
            "guest_hooks": [],
            "cleanup": [slice.to_string_lossy()]
        }
    });
    let body = serde_json::to_vec(&response).expect("prepare serializes");
    if write_frame(&mut stdout_lock, &body).is_err() {
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
