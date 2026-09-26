//! Landlock extension guest binary (task 3.1).
//!
//! The external `prepare` guest: speaks the versioned stdio
//! protocol on stdin/stdout, answers one prepare transaction per
//! session with the Landlock guest-hook request, and announces the
//! `landlock` role capability at hello. The hook names the shipped
//! wrapper by digest-pinned registry reference; the framework
//! resolves, verifies, and stages it (task 3.2). Stderr carries
//! human-readable notes only; protocol bytes never leave stdout
//! except as length-prefixed frames.
//!
//! Fail-closed: a missing or unreadable wrapper binary exits
//! nonzero WITHOUT answering prepare, so no session starts
//! believing itself confined. The host reads EOF as its own typed
//! pre-execute error.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use cistella::framework::contract::{
    HOOK_ARTIFACT_KIND_BLOB, HOOK_ON_FAILURE_PRE_EXEC, HOOK_STAGING_ISOLATOR,
};
use cistella::framework::protocol::{
    DEFAULT_MAX_FRAME, HOST_MAX_FRAME, PRE_NEGOTIATION_MAX_FRAME, PROTOCOL_MAJOR, envelope_bytes,
    parse_envelope, read_frame, write_frame,
};
use cistella::framework::registry::{
    LANDLOCK_PROBE_OP, SHIPPED_REGISTRY_ID, STAGED_WRAPPER_GUEST_PATH, WRAPPER_FILE_NAME,
    digest_sibling,
};

/// Role capability announced at hello (also the closed-negotiation
/// name conduct offers, alongside the contribution type).
const ROLE_CAPABILITY: &str = "landlock";

/// Contribution capability the hook request exercises.
const HOOKS_CAPABILITY: &str = "guest-hooks";

/// First-hello wait budget (the host drives promptly; eternity here
/// would wedge installs that never speak).
const HELLO_WAIT: Duration = Duration::from_secs(30);

/// Prepare-answer budget (framework-owned deadline mirrors this).
const OP_BUDGET: Duration = Duration::from_secs(120);

/// Guest-context probe budget, milliseconds.
const PROBE_TIMEOUT_MS: u64 = 10_000;

/// Writes one response envelope frame; a broken pipe ends the guest
/// (the host reads EOF as its own typed error).
fn send<W: Write>(
    stdout: &mut W,
    id: &str,
    op: &str,
    payload: serde_json::Value,
    max_frame: usize,
) -> Result<(), ExitCode> {
    let envelope = cistella::framework::protocol::Envelope {
        protocol: PROTOCOL_MAJOR,
        id: id.to_string(),
        op: op.to_string(),
        payload,
    };
    let body = envelope_bytes(&envelope);
    write_frame(stdout, &body, max_frame).map_err(|_| ExitCode::from(2))
}

/// Answers one prepare request: empty env/mount/claim/credential
/// sets plus the single Landlock hook. The wrapper digest is
/// observed from the sibling install directory at answer time, so
/// the advertised bytes are exactly the shipped bytes.
fn answer_prepare(exe_dir: &std::path::Path) -> Result<serde_json::Value, ExitCode> {
    let sha256 = digest_sibling(exe_dir, WRAPPER_FILE_NAME).map_err(|error| {
        eprintln!("error: wrapper unavailable: {error}");
        ExitCode::from(2)
    })?;
    Ok(serde_json::json!({
        "environment": [],
        "mounts": [],
        "policy_claims": [],
        "guest_hooks": [{
            "artifact": {
                "kind": HOOK_ARTIFACT_KIND_BLOB,
                "sha256": sha256,
                "source": {"registry": SHIPPED_REGISTRY_ID, "path": WRAPPER_FILE_NAME},
            },
            "staging": HOOK_STAGING_ISOLATOR,
            "order": 0,
            "argv_prefix": [STAGED_WRAPPER_GUEST_PATH],
            "probe": {"op": LANDLOCK_PROBE_OP, "timeout_ms": PROBE_TIMEOUT_MS},
            "on_failure": HOOK_ON_FAILURE_PRE_EXEC,
        }],
        "credentials": [],
    }))
}

fn main() -> ExitCode {
    let exe_dir = match std::env::current_exe() {
        Ok(exe) => match exe.parent() {
            Some(dir) => dir.to_path_buf(),
            None => return ExitCode::from(2),
        },
        Err(_) => return ExitCode::from(2),
    };
    let stdout = std::io::stdout();
    // Unbuffered stdin ownership: same lost-wakeup discipline as the
    // isolator guest (see its main); an owned `File` reads exactly
    // what framing asks for.
    use std::os::fd::FromRawFd;
    if nix::fcntl::fcntl(0, nix::fcntl::FcntlArg::F_GETFD).is_err() {
        return ExitCode::from(2);
    }
    // SAFETY: fd 0 is open (checked above) and owned by this
    // process image as its stdin for the process lifetime; no
    // other owner reads it, and the `File` outlives `main`.
    let mut reader = unsafe { std::fs::File::from_raw_fd(0) };

    // Hello under the pre-negotiation ceiling: version must match
    // before any planning, and the request must actually be hello.
    let hello_body = match read_frame(&mut reader, PRE_NEGOTIATION_MAX_FRAME, HELLO_WAIT) {
        Ok(body) => body,
        Err(_) => return ExitCode::from(2),
    };
    let hello = match parse_envelope(&hello_body) {
        Ok(envelope) => envelope,
        Err(_) => return ExitCode::from(2),
    };
    if hello.op != "hello" || hello.protocol != PROTOCOL_MAJOR {
        return ExitCode::from(2);
    }
    let response = cistella::framework::protocol::Envelope {
        protocol: PROTOCOL_MAJOR,
        id: hello.id.clone(),
        op: "hello".to_string(),
        payload: serde_json::json!({
            "version": PROTOCOL_MAJOR,
            "capabilities": [ROLE_CAPABILITY, HOOKS_CAPABILITY],
            "max_frame": HOST_MAX_FRAME as u32,
        }),
    };
    let max_frame = DEFAULT_MAX_FRAME;
    let body = envelope_bytes(&response);
    {
        let mut writer = stdout.lock();
        if write_frame(&mut writer, &body, PRE_NEGOTIATION_MAX_FRAME).is_err() {
            return ExitCode::from(2);
        }
    }
    loop {
        // Connection discipline mirrors the isolator guest: a clean
        // EOF at a frame boundary exits quietly (the host shuts us
        // down after the prepare transaction); anything else is a
        // protocol failure. Prepare answers are idempotent — the
        // digest is re-observed per answer, never cached.
        let frame = match read_frame(&mut reader, max_frame, OP_BUDGET) {
            Ok(body) => body,
            Err(error) if cistella::framework::protocol::is_clean_eof(&error) => {
                return ExitCode::SUCCESS;
            }
            Err(error) if cistella::framework::protocol::is_read_timeout(&error) => continue,
            Err(_) => return ExitCode::from(2),
        };
        let envelope = match parse_envelope(&frame) {
            Ok(envelope) => envelope,
            Err(_) => return ExitCode::from(2),
        };
        // A second hello is a protocol violation, and only prepare
        // is served: anything else exits nonzero.
        if envelope.op != "prepare" {
            return ExitCode::from(2);
        }
        let payload = match answer_prepare(&exe_dir) {
            Ok(payload) => payload,
            Err(code) => return code,
        };
        let mut writer = stdout.lock();
        if send(&mut writer, &envelope.id, "prepare", payload, max_frame).is_err() {
            return ExitCode::from(2);
        }
    }
}
