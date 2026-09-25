//! Deterministic protocol peer for conformance harness fault injection.
//!
//! Spawned by `framework::protocol::GuestHost` exactly like a real
//! extension/isolator helper: a pinned executable that produces
//! scripted fault shapes at the stdio boundary. Implements zero
//! lifecycle meaning — never satisfies a lifecycle-common test
//! (task 3.1 applicability rule #1). The peer proves that the host
//! refuses or recovers deterministically; the host is what the suite
//! tests, not the peer.
//!
//! Behavior is selected by argv flags (the protocol host calls
//! `GuestHost::spawn(absolute_path, args, deadlines)`, so the path is
//! already pinned and behavior stays scriptable through argv):
//!
//!   `--mode=hello-version-mismatch`     hello with wrong `version`
//!   `--mode=hello-bad-capability`       hello with unknown capability name
//!   `--mode=hello-real-capabilities`    hello advertising the five real capability names
//!   `--mode=hello-evil-capability`      hello with control bytes in the capability name
//!   `--mode=isolator-concurrent`        scripted isolator ops: slow await plus concurrent op
//!   `--mode=isolator-split-frame`       scripted isolator op split across slices
//!   `--mode=isolator-wrong-op`          scripted isolator reply with mismatched op
//!   `--mode=isolator-unsolicited`       scripted isolator reply to an unknown id
//!   `--mode=isolator-big-frame`         scripted isolator reply between 1 MiB and ceiling
//!   `--mode=isolator-split-frame`       scripted isolator op split across slices
//!   `--mode=hello-then-eof`             hello, then close stdin
//!   `--mode=malformed-frame-header`     send a header that exceeds negotiated max
//!   `--mode=oversize-frame`             declare length far above `PRE_NEGOTIATION_MAX_FRAME`
//!   `--mode=unknown-fields`             envelope with `deny_unknown_fields` violation
//!   `--mode=duplicate-id`               echo two responses with the same id
//!   `--mode=pending-on-solo`            send `{pending: true}` on a single-shot exchange
//!   `--mode=hang-hello`                 sleep forever before hello
//!   `--mode=hang-request`               hello, then sleep forever
//!   `--mode=partial-response`           declare body length, deliver less
//!   `--mode=spurious-after-terminal`    hello + valid response, then send a stray frame
//!   `--mode=cleanup-then-write`         hello, kill own stdin, try to write to stdout
//!   `--mode=stderr-fill`                write to stderr past `STDERR_CAP` then exit
//!   `--mode=prepare-vector-agentmux`      Agentmux-support design vector (task 3.3)
//!   `--mode=prepare-vector-ssh`           SSH design vector (task 3.3)
//!   `--mode=prepare-vector-token`         token-shaped env from an extension (task 3.3)
//!   `--mode=prepare-vector-token-weakening` weakening claim over token pattern (task 3.3)
//!   `--mode=normal-echo`                hello + echo requests one-for-one (default)
//!
//! Mode defaults to `normal-echo` for unknown values, which itself
//! proves the host treats the peer as untrusted input and refuses any
//! unexpected shape.
//!
//! Cargo treats this file as a test target only (see `Cargo.toml`'s
//! `[[test]]` block); `package.include` excludes `tests/**`, so the
//! peer never ships to crates.io.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

const PROTOCOL_MAJOR: u32 = 1;

mod modes_fault;
mod modes_hello;
mod modes_isolator;
mod modes_prepare;

fn read_arg_mode() -> String {
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--mode=") {
            return value.to_string();
        }
    }
    "normal-echo".to_string()
}

/// Reads `--fd-watch=DIR` argv (rendezvous directory to connect
/// and hold for client-concurrency tests), if present.
fn read_fd_watch() -> Option<String> {
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--fd-watch=") {
            return Some(value.to_string());
        }
    }
    None
}

/// Connects to the first rendezvous socket in `dir` and holds it
/// open for the process lifetime: proves the guest side of
/// pid-bound accept without moving any bundles (concurrency tests
/// never launch). Uses a raw `SOCK_SEQPACKET` socket to match the
/// rendezvous listener type (`UnixStream` is stream-oriented and
/// cannot connect to it).
fn hold_rendezvous(dir: &str) {
    // SAFETY: libc socket/connect with a valid pathname; the
    // connected fd is leaked intentionally (held open for the
    // process lifetime, exactly the cooperation under test).
    unsafe fn connect_one(path: &std::path::Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        // SAFETY: straight-line libc calls with checked return
        // values; the connected fd is intentionally never closed
        // (held open for the process lifetime).
        unsafe {
            let fd = ::libc::socket(::libc::AF_UNIX, ::libc::SOCK_SEQPACKET, 0);
            if fd < 0 {
                return false;
            }
            let bytes = path.as_os_str().as_bytes();
            let mut addr: ::libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = ::libc::AF_UNIX as ::libc::sa_family_t;
            if bytes.len() + 1 > addr.sun_path.len() {
                ::libc::close(fd);
                return false;
            }
            for (slot, byte) in addr.sun_path.iter_mut().zip(bytes.iter()) {
                *slot = *byte as ::libc::c_char;
            }
            let connected = ::libc::connect(
                fd,
                &addr as *const _ as *const ::libc::sockaddr,
                (std::mem::size_of::<::libc::sa_family_t>() + bytes.len() + 1) as ::libc::socklen_t,
            ) == 0;
            if !connected {
                ::libc::close(fd);
            }
            connected
        }
    }
    unsafe {
        for _ in 0..200 {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let mut names: Vec<_> = entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sock"))
                    .collect();
                names.sort();
                for path in names {
                    if connect_one(&path) {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn protocol_error_exit() -> ExitCode {
    ExitCode::from(2)
}

/// Minimal length-prefixed frame writer that respects the pre-negotiation
/// ceiling. Mirrors `framework::protocol::write_frame`'s shape so the
/// host's framing code is exercised symmetrically.
fn write_frame<W: Write>(stdout: &mut W, payload: &[u8], max_frame: usize) -> std::io::Result<()> {
    if payload.len() > max_frame {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("frame length {} exceeds {max_frame}", payload.len()),
        ));
    }
    let header = (payload.len() as u32).to_be_bytes();
    stdout.write_all(&header)?;
    stdout.write_all(payload)?;
    stdout.flush()
}

fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    reader.read_exact(buf)
}

/// Reads one length-prefixed frame from stdin (header + body). Returns
/// the body bytes when the host's wire shape is correct, or `Err` when
/// the pipe is shorter than the declared length (host closed early or
/// wire shape is malformed). Used by fault modes that need to keep
/// stdin byte-aligned with the host's send/recv cadence.
fn read_frame_from_stdin<R: Read>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; HEADER_LEN];
    read_exact(reader, &mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    let mut body = vec![0u8; len];
    read_exact(reader, &mut body)?;
    Ok(body)
}

fn hello_response_ok() -> Value {
    json!({
        "protocol": PROTOCOL_MAJOR,
        "id": "hello",
        "op": "hello",
        "payload": {
            "version": PROTOCOL_MAJOR,
            "capabilities": ["test-cap"],
            "max_frame": 1024u32
        }
    })
}

const HEADER_LEN: usize = 4;

mod libc {
    use std::ffi::c_void;
    unsafe extern "C" {
        pub unsafe fn write(fd: i32, buf: *const c_void, count: usize) -> isize;
        pub unsafe fn close(fd: i32) -> i32;
    }
}

/// Build one request response matching the host's wire envelope shape,
/// then write it as a length-prefixed frame. `op` is echoed from the
/// request envelope; `payload` is whatever the test wants the peer
/// to assert against. Currently unused (kept for symmetry with
/// `send_prepare_with_payload`'s future variants and for direct test
/// composition if a fault needs a hand-built envelope).
#[expect(dead_code)]
fn write_envelope_response(
    stdout: &mut std::io::Stdout,
    id: &str,
    op: &str,
    payload: Value,
    max_frame: usize,
) -> std::io::Result<()> {
    let envelope = json!({
        "protocol": PROTOCOL_MAJOR,
        "id": id,
        "op": op,
        "payload": payload,
    });
    let body = serde_json::to_vec(&envelope).expect("envelope serializes");
    write_frame(stdout, &body, max_frame)
}
fn main() -> ExitCode {
    let mode = read_arg_mode();
    if let Some(dir) = read_fd_watch() {
        // Rendezvous cooperation for client-concurrency tests:
        // connect in the background while the mode serves ops.
        std::thread::spawn(move || hold_rendezvous(&dir));
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    for run in [
        modes_hello::run,
        modes_fault::run,
        modes_prepare::run,
        modes_isolator::run,
    ] {
        if let Some(code) = run(&mode, &mut stdin_lock, &mut stdout_lock) {
            return code;
        }
    }
    // Unknown mode falls back to `normal-echo` behavior —
    // the host treats the peer as untrusted input and any
    // deviation from the negotiated protocol must surface
    // as a typed error rather than a silent success.
    let mut header = [0u8; 4];
    if read_exact(&mut stdin_lock, &mut header).is_err() {
        return protocol_error_exit();
    }
    let body1 = serde_json::to_vec(&hello_response_ok()).expect("serialize");
    let _ = write_frame(&mut stdout_lock, &body1, 64 * 1024);
    ExitCode::SUCCESS
}
