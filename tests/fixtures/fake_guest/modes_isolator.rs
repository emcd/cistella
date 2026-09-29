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
        "isolator-await-stall" => {
            // Hello, then one header byte followed by silence:
            // the dispatcher's frame-completion bound (not its
            // op deadline) must fail the pending call typed.
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
            let _ = stdout_lock.write_all(&[0u8; 1]);
            let _ = stdout_lock.flush();
            std::thread::sleep(Duration::from_secs(60));
            ExitCode::SUCCESS
        }
        "isolator-exit-after-hello" => {
            // Hello, then exit promptly: exercises close() against
            // an already-dead dispatcher (join + path removal must
            // run on every close outcome, not just clean ones).
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
            std::thread::sleep(Duration::from_millis(500));
            ExitCode::SUCCESS
        }
        "isolator-die-after-two" => {
            // Hello, then die abruptly after the SECOND op arrives,
            // replying to neither: both callers must be in flight
            // simultaneously at the moment of death (no sleep-tuned
            // sequencing on the test side — the peer itself waits
            // for both frames before dying). The client must fail
            // both with typed errors and trip its death latch.
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
            let _ = read_frame_from_stdin(&mut stdin_lock);
            std::process::exit(1);
        }
        "isolator-create-stall" => {
            // Hello, then stall 30s without reading: a large op
            // (over the pipe buffer) blocks in write past any
            // bounded apply deadline with partial bytes already
            // emitted. The client must treat the send timeout as
            // fatal — bounded shutdown/reap first, then latch and
            // fail typed — since neither end resynchronizes a
            // timed-out stream. Killed by the worker's shutdown;
            // the long sleep only bounds a shutdown failure.
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
            std::thread::sleep(Duration::from_secs(30));
            ExitCode::SUCCESS
        }
        "isolator-small-ceiling" => {
            // Hello with a 256-byte ceiling, then serve state
            // normally: a create payload (hundreds of bytes of
            // session spec) exceeds the ceiling the guest itself
            // promised, so the client must refuse it LOCALLY with a
            // typed Contract error — never sent, guest untouched,
            // latch clear — while a small state op still succeeds
            // through the same dispatcher afterwards.
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
                    "max_frame": 256u32
                }
            }))
            .expect("serialize");
            let _ = write_frame(&mut stdout_lock, &hello, 256);
            loop {
                let op_body = match read_frame_from_stdin(&mut stdin_lock) {
                    Ok(body) => body,
                    Err(_) => return Some(protocol_error_exit()),
                };
                let (id, op) = serde_json::from_slice::<Value>(&op_body)
                    .ok()
                    .map(|env| {
                        (
                            env.get("id").cloned().unwrap_or(json!("small-0")),
                            env.get("op")
                                .and_then(|op| op.as_str())
                                .unwrap_or("")
                                .to_string(),
                        )
                    })
                    .unwrap_or((json!("small-0"), String::new()));
                if op != "isolator.state" {
                    return Some(protocol_error_exit());
                }
                let response = serde_json::to_vec(&json!({
                    "protocol": PROTOCOL_MAJOR,
                    "id": id,
                    "op": "isolator.state",
                    "payload": {"ok": {"lifecycle": "initiated"}}
                }))
                .expect("serialize");
                let _ = write_frame(&mut stdout_lock, &response, 256);
            }
        }
        "isolator-lingering-descendant" => {
            // Hello, read one op frame (left pending forever), then
            // fork a setsid grandchild holding stdout while the
            // parent exits immediately: the framework's group kill
            // (SIGTERM, grace, SIGKILL to the parent's group) cannot
            // touch the reparented setsid child, so pipe-EOF
            // verification fails its budget and shutdown reports
            // residue-class. The client must fail WITHOUT latching
            // death (the descendant may live) and name the shutdown
            // failure. Child uses raw libc only (fork in a
            // multithreaded process): setsid, close stdin, sleep,
            // _exit. Bounded 10s self-exit; nothing leaks past it.
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
            let _ = stdout_lock.flush();
            let forked = unsafe { ::libc::fork() };
            if forked < 0 {
                return Some(protocol_error_exit());
            }
            if forked == 0 {
                // Child: escape the group kill, hold stdout, leave
                // stdin fully closed (parent exit closes the last
                // other read end, so later sends fail fast). Raw
                // libc only; single sleep (no signal source exists
                // for this reparented child — our group kill misses
                // its new pgid by construction).
                unsafe {
                    let _ = ::libc::setsid();
                    ::libc::close(0);
                    let remaining = ::libc::timespec {
                        tv_sec: 10,
                        tv_nsec: 0,
                    };
                    let _ = ::libc::nanosleep(&remaining, std::ptr::null_mut());
                    ::libc::_exit(0);
                }
            }
            unsafe { ::libc::_exit(0) };
        }
        _ => return None,
    })
}
