//! Podman isolator guest binary (task 2.1).
//!
//! The external `isolator.*` guest: speaks the versioned stdio
//! protocol on stdin/stdout, dispatches every op through the shared
//! [`cistella::isolators::wire::IsolatorGuest`] dispatch (same
//! semantics as the in-process reference, no drift), and announces
//! the `isolator` role capability at hello. Stderr carries
//! human-readable notes only; protocol bytes never leave stdout
//! except as length-prefixed frames.
//!
//! Response envelopes mirror the isolator-contract schemas:
//! `{"ok": <payload>}` terminal success, `{"pending": true}`
//! ticker frames during `await_result`, then exactly one terminal
//! frame; `{"error": {"code", "message"}}` typed failure with a
//! value-free message. The framework client (task 2.2) redeems
//! these; EOF on stdin exits cleanly, and a broken stdout exits
//! nonzero (the host reads EOF as its own typed error).

use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use cistella::error::CistellaError;
use cistella::framework::protocol::{
    AWAIT_RESULT_OP, DEFAULT_MAX_FRAME, HOST_MAX_FRAME, PRE_NEGOTIATION_MAX_FRAME, PROTOCOL_MAJOR,
    envelope_bytes, parse_envelope, read_frame, write_frame,
};
use cistella::isolators::wire::IsolatorGuest;

/// Role capability announced at hello (also the closed-negotiation
/// name conduct offers).
const ROLE_CAPABILITY: &str = "isolator";

/// First-hello wait budget (the host drives promptly; eternity here
/// would wedge installs that never speak).
const HELLO_WAIT: Duration = Duration::from_secs(30);

/// `{pending}` ticker cadence during `await_result`.
const AWAIT_TICK: Duration = Duration::from_secs(2);

/// Guest-side op budget (framework-owned deadlines mirror these;
/// the guest never waits unboundedly except inside await).
const OP_BUDGET: Duration = Duration::from_secs(120);

/// SIGTERM grace for disconnect-time convergence sweeps.
const CONVERGE_GRACE: Duration = Duration::from_secs(30);

/// Runs the disconnect-time convergence sweep, reporting cleanup
/// failure on stderr (human notes only, never protocol): exit 2
/// must distinguish incomplete cleanup from a malformed frame.
fn converge_report(guest: &IsolatorGuest) {
    if let Err(error) = guest.converge_all(CONVERGE_GRACE) {
        eprintln!("error: disconnect converge left residue: {error}");
    }
}

/// Maps an error to its wire `code` string.
fn error_code(error: &CistellaError) -> &'static str {
    match error {
        CistellaError::Profile(_) => "profile",
        CistellaError::Mount(_) => "mount",
        CistellaError::Runtime(_) => "runtime",
        CistellaError::Transport(_) => "transport",
        CistellaError::Contract(_) => "contract",
        CistellaError::Protocol(_) => "protocol",
        CistellaError::Identity(_) => "identity",
        CistellaError::Preflight(_) => "preflight",
        CistellaError::LockContended => "lock-contended",
        CistellaError::Selector(_) => "selector",
        CistellaError::Detached(_) => "detached",
        CistellaError::Io(_) => "io",
    }
}

/// Renders a dispatch outcome as its terminal response payload.
fn terminal_payload(result: Result<serde_json::Value, CistellaError>) -> serde_json::Value {
    match result {
        Ok(payload) => serde_json::json!({"ok": payload}),
        Err(error) => serde_json::json!({"error": {
            "code": error_code(&error),
            "message": error.to_string(),
        }}),
    }
}

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

/// Serves one `await_result` without blocking the op loop: a detached
/// worker thread runs the blocking backend wait while emitting
/// `{pending}` ticker frames, then exactly one terminal frame (no
/// frames after terminal). The main loop keeps serving
/// terminate/remove/inspect during the wait; the worker shares the
/// guest tables (blocking backend calls run lock-free after handle
/// resolution) and the writer mutex.
///
/// Both threads are fully detached: when a send fails (host
/// abandonment), the ticker loop ends at once without joining the
/// waiter — abandonment detaches without killing, the execution
/// keeps running, and the waiter's eventual outcome send fails
/// harmlessly into the dropped channel. The worker never cancels
/// the wait and never fabricates an outcome. Re-await after a guest
/// restart is not yet supported — full cross-crash execution
/// survival stays deferred.
fn serve_await(
    writer: &std::sync::Arc<std::sync::Mutex<std::io::Stdout>>,
    guest: &std::sync::Arc<IsolatorGuest>,
    id: &str,
    payload: &serde_json::Value,
    max_frame: usize,
) {
    let writer = std::sync::Arc::clone(writer);
    let guest = std::sync::Arc::clone(guest);
    let id = id.to_string();
    let payload = payload.clone();
    std::thread::spawn(move || {
        let (sender, receiver) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result = guest.dispatch(AWAIT_RESULT_OP, &payload);
            let _ = sender.send(result);
        });
        let _ = waiter;
        loop {
            match receiver.recv_timeout(AWAIT_TICK) {
                Ok(result) => {
                    let mut stdout = match writer.lock() {
                        Ok(stdout) => stdout,
                        Err(_) => return,
                    };
                    let _ = send(
                        &mut *stdout,
                        &id,
                        AWAIT_RESULT_OP,
                        terminal_payload(result),
                        max_frame,
                    );
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let mut stdout = match writer.lock() {
                        Ok(stdout) => stdout,
                        Err(_) => return,
                    };
                    if send(
                        &mut *stdout,
                        &id,
                        AWAIT_RESULT_OP,
                        serde_json::json!({"pending": true}),
                        max_frame,
                    )
                    .is_err()
                    {
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}

fn main() -> ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();

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
            "capabilities": [ROLE_CAPABILITY],
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

    let guest = {
        // Optional `--fd-socket <path>`: connect the ancillary-fd
        // channel before serving ops. Without it the guest serves
        // every op except launch (which refuses: harness stdio has
        // no channel to arrive on).
        let mut channel = None;
        let argv: Vec<String> = std::env::args().collect();
        let mut position = 1;
        while position < argv.len() {
            if argv[position] == "--fd-socket" && position + 1 < argv.len() {
                let path = std::path::PathBuf::from(&argv[position + 1]);
                match cistella::framework::fdpass::connect_rendezvous(&path) {
                    Ok(sock) => channel = Some(sock),
                    Err(_) => return ExitCode::from(2),
                }
                position += 2;
            } else {
                position += 1;
            }
        }
        let guest = IsolatorGuest::new();
        let guest = match channel {
            Some(sock) => guest.with_fd_channel(sock),
            None => guest,
        };
        std::sync::Arc::new(guest)
    };
    let writer = std::sync::Arc::new(std::sync::Mutex::new(std::io::stdout()));
    loop {
        // Connection discipline: only a clean EOF at a frame
        // boundary with no live state exits quietly, and only real
        // protocol failures converge and exit 2. Idle timeouts loop
        // again: the framework owns guest lifetime and re-hosts
        // nothing mid-session, so silence is patience, not failure
        // and never stops the guest.
        let frame = match read_frame(&mut reader, max_frame, OP_BUDGET) {
            Ok(body) => body,
            Err(error)
                if cistella::framework::protocol::is_clean_eof(&error)
                    && !guest.has_live_state() =>
            {
                return ExitCode::SUCCESS;
            }
            Err(error) if cistella::framework::protocol::is_read_timeout(&error) => continue,
            Err(_) => {
                converge_report(&guest);
                return ExitCode::from(2);
            }
        };
        let envelope = match parse_envelope(&frame) {
            Ok(envelope) => envelope,
            Err(_) => {
                converge_report(&guest);
                return ExitCode::from(2);
            }
        };
        // A second hello is a protocol violation: the session
        // negotiated exactly once, and re-negotiation would fork
        // the capability/max-frame agreement mid-stream.
        if envelope.op == "hello" {
            converge_report(&guest);
            return ExitCode::from(2);
        }
        if envelope.op == AWAIT_RESULT_OP {
            serve_await(&writer, &guest, &envelope.id, &envelope.payload, max_frame);
            continue;
        }
        let result = guest.dispatch(&envelope.op, &envelope.payload);
        {
            let mut stdout = match writer.lock() {
                Ok(stdout) => stdout,
                Err(_) => {
                    converge_report(&guest);
                    return ExitCode::from(2);
                }
            };
            if send(
                &mut *stdout,
                &envelope.id,
                &envelope.op,
                terminal_payload(result),
                max_frame,
            )
            .is_err()
            {
                drop(stdout);
                converge_report(&guest);
                return ExitCode::from(2);
            }
        }
    }
}
