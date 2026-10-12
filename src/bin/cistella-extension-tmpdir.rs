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
//! undeclared session.
//!
//! Shipped sibling binary (autobins): installed and discovered
//! beside the driver like `cistella-extension-landlock`.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
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

/// Mints the session slice: fixed prefix plus session id plus
/// process id plus nanos, exclusive create, owner-only mode. Any
/// failure fails loudly (the prepare must refuse, never run
/// undeclared). The parent is the seat temp root (`$TMPDIR` when
/// set and absolute, else `/tmp`) — standard temp conventions,
/// so confined seats and capacity redirects work; gc enumerates
/// the same computed root.
fn mint_slice(session: &str) -> std::io::Result<std::path::PathBuf> {
    let root = std::env::var("TMPDIR")
        .ok()
        .filter(|dir| !dir.is_empty() && dir.starts_with('/'))
        .unwrap_or_else(|| "/tmp".to_string());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let name = format!("cistella-tmp-{session}-{}-{nanos}", std::process::id());
    let path = std::path::PathBuf::from(root).join(name);
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

    // Hello: consume the host's hello, advertise the role plus
    // the contribution types (the merge gate admits nonempty
    // contribution types only when advertised).
    if read_frame(&mut stdin_lock).is_err() {
        return ExitCode::from(2);
    }
    let hello = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": "hello",
        "op": "hello",
        "payload": {
            "version": PROTOCOL_MAJOR,
            "capabilities": [role, "mounts", "cleanup"],
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
    let id = request.get("id").cloned().unwrap_or(json!("req-0"));
    let session = request
        .get("payload")
        .and_then(|payload| payload.get("session_id"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
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
                    "container_target": "/tmp",
                    "mode": "rw"
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
