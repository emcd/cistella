//! Shipped ssh guest pins: hello advertisement, exact-tuple
//! prepare, and the refusal matrix, all against the real
//! `cistella-extension-ssh` sibling binary over scripted frames
//! (the `protocol_peer` tmpdir pattern). Sockets are real
//! `UnixListener` binds; the P!=C case is a symlink onto one.

use std::io::{Read, Write};

fn shipped_bin(name: &str) -> std::path::PathBuf {
    std::env::current_exe()
        .expect("current_exe")
        .ancestors()
        .nth(2)
        .expect("target/<profile> ancestors")
        .join(name)
}

fn framed(op: &str, protocol: u32, payload: serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(&serde_json::json!({
        "protocol": protocol,
        "id": "req-0",
        "op": op,
        "payload": payload,
    }))
    .expect("frame serializes");
    let mut framed = (body.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(&body);
    framed
}

fn read_frames(mut stdout: &[u8]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    while stdout.len() >= 4 {
        let len = u32::from_be_bytes(stdout[..4].try_into().expect("header")) as usize;
        let body = &stdout[4..4 + len];
        out.push(serde_json::from_slice(body).expect("frame parses"));
        stdout = &stdout[4 + len..];
    }
    out
}

struct Transcript {
    code: Option<i32>,
    frames: Vec<serde_json::Value>,
}

fn run_guest(args: &[&str], writes: Vec<Vec<u8>>) -> Transcript {
    let mut child = std::process::Command::new(shipped_bin("cistella-extension-ssh"))
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("ssh guest spawns");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for frame in &writes {
            stdin.write_all(frame).expect("frame writes");
        }
    }
    drop(child.stdin.take());
    let mut stdout = Vec::new();
    child
        .stdout
        .as_mut()
        .expect("stdout")
        .read_to_end(&mut stdout)
        .expect("stdout drains");
    let output = child.wait().expect("guest exits");
    Transcript {
        code: output.code(),
        frames: read_frames(&stdout),
    }
}

fn hello_frame() -> Vec<u8> {
    framed(
        "hello",
        2,
        serde_json::json!({"version": 2, "capabilities": []}),
    )
}

fn prepare_frame(session: serde_json::Value) -> Vec<u8> {
    framed("prepare", 2, session)
}

fn fixture_socket(dir: &tempfile::TempDir, name: &str) -> String {
    let path = dir.path().join(name);
    std::os::unix::net::UnixListener::bind(&path).expect("socket binds");
    path.to_string_lossy().into_owned()
}

#[test]
fn ssh_hello_advertises_role_and_contribution_types() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let sock = fixture_socket(&dir, "agent.sock");
    let argv_p = format!("--socket={sock}");
    let argv_c = format!("--socket-canonical={sock}");
    let transcript = run_guest(
        &[&argv_p, &argv_c],
        vec![
            hello_frame(),
            prepare_frame(serde_json::json!({"session_id": "s-hello"})),
        ],
    );
    assert_eq!(transcript.code, Some(0), "happy path exits zero");
    assert_eq!(transcript.frames.len(), 2, "hello + prepare answers");
    let caps = transcript.frames[0]
        .pointer("/payload/capabilities")
        .expect("hello carries capabilities");
    assert_eq!(
        caps,
        &serde_json::json!(["ssh", "environment", "mounts", "permissions", "credentials"]),
        "role plus the four returned types, nothing else"
    );
    let max_frame = transcript.frames[0]
        .pointer("/payload/max_frame")
        .expect("hello carries max_frame");
    assert_eq!(
        max_frame,
        &serde_json::json!(65536),
        "advertised bound covers the largest serializable tuple"
    );
}

#[test]
fn ssh_prepare_contributes_exact_tuple() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let sock = fixture_socket(&dir, "agent.sock");
    let argv_p = format!("--socket={sock}");
    let argv_c = format!("--socket-canonical={sock}");
    let transcript = run_guest(
        &[&argv_p, &argv_c],
        vec![
            hello_frame(),
            prepare_frame(serde_json::json!({"session_id": "s-tuple"})),
        ],
    );
    assert_eq!(transcript.code, Some(0));
    let payload = transcript.frames[1]
        .pointer("/payload")
        .expect("prepare payload");
    assert_eq!(
        payload.pointer("/environment"),
        Some(&serde_json::json!([{"name": "SSH_AUTH_SOCK", "value": sock}])),
        "env names the declared path"
    );
    assert_eq!(
        payload.pointer("/mounts"),
        Some(&serde_json::json!([{"host_source": sock, "container_target": sock}])),
        "self-bind read-only shape"
    );
    assert_eq!(
        payload.pointer("/permissions"),
        Some(&serde_json::json!([{"target": sock, "rights": "read"}])),
        "permission names the guest-visible target"
    );
    assert_eq!(
        payload.pointer("/credentials"),
        Some(&serde_json::json!([{"kind": "seat-socket", "path": sock}])),
        "credential names the canonical path"
    );
    assert_eq!(
        payload.pointer("/policy_claims"),
        Some(&serde_json::json!([])),
        "no claims"
    );
    assert_eq!(
        payload.pointer("/guest_hooks"),
        Some(&serde_json::json!([])),
        "no hooks"
    );
}

#[test]
fn ssh_symlink_splits_declared_from_canonical() {
    // P!=C: declared P is a symlink onto canonical C. Env,
    // container target, and permission name P; mount source and
    // credential name C.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let canonical = fixture_socket(&dir, "real.sock");
    let declared = dir.path().join("link.sock");
    std::os::unix::fs::symlink(&canonical, &declared).expect("symlink");
    let declared = declared.to_string_lossy().into_owned();
    let argv_p = format!("--socket={declared}");
    let argv_c = format!("--socket-canonical={canonical}");
    let transcript = run_guest(
        &[&argv_p, &argv_c],
        vec![
            hello_frame(),
            prepare_frame(serde_json::json!({"session_id": "s-split"})),
        ],
    );
    assert_eq!(transcript.code, Some(0));
    let payload = transcript.frames[1]
        .pointer("/payload")
        .expect("prepare payload");
    assert_eq!(
        payload.pointer("/environment"),
        Some(&serde_json::json!([{"name": "SSH_AUTH_SOCK", "value": declared}])),
        "env names declared P"
    );
    assert_eq!(
        payload.pointer("/mounts"),
        Some(&serde_json::json!([{"host_source": canonical, "container_target": declared}])),
        "mount binds canonical C onto declared P"
    );
    assert_eq!(
        payload.pointer("/permissions"),
        Some(&serde_json::json!([{"target": declared, "rights": "read"}])),
        "permission names guest-visible P, never host C"
    );
    assert_eq!(
        payload.pointer("/credentials"),
        Some(&serde_json::json!([{"kind": "seat-socket", "path": canonical}])),
        "credential names canonical C"
    );
}

#[test]
fn ssh_legacy_hello_refuses_silent() {
    let transcript = run_guest(
        &[],
        vec![framed(
            "hello",
            1,
            serde_json::json!({"version": 1, "capabilities": []}),
        )],
    );
    assert_eq!(transcript.code, Some(2), "legacy hello refuses");
    assert!(
        transcript.frames.is_empty(),
        "refusing guest answers nothing"
    );
}

#[test]
fn ssh_prepare_without_session_refuses() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let sock = fixture_socket(&dir, "agent.sock");
    let argv_p = format!("--socket={sock}");
    let argv_c = format!("--socket-canonical={sock}");
    let transcript = run_guest(
        &[&argv_p, &argv_c],
        vec![hello_frame(), prepare_frame(serde_json::json!({}))],
    );
    assert_eq!(transcript.code, Some(2), "missing session refuses");
    assert_eq!(
        transcript.frames.len(),
        1,
        "hello answer only, no prepare answer"
    );
}

#[test]
fn ssh_missing_argv_refuses() {
    // No --socket/--socket-canonical at all: prepare refuses
    // after hello (argv-gated, never filesystem-gated).
    let transcript = run_guest(
        &[],
        vec![
            hello_frame(),
            prepare_frame(serde_json::json!({"session_id": "s-noargv"})),
        ],
    );
    assert_eq!(transcript.code, Some(2), "missing argv refuses");
    assert_eq!(transcript.frames.len(), 1);
}

#[test]
fn ssh_absent_socket_refuses() {
    let missing = "/nonexistent-cistella-ssh-test/agent.sock";
    let argv_p = format!("--socket={missing}");
    let argv_c = format!("--socket-canonical={missing}");
    let transcript = run_guest(
        &[&argv_p, &argv_c],
        vec![
            hello_frame(),
            prepare_frame(serde_json::json!({"session_id": "s-absent"})),
        ],
    );
    assert_eq!(transcript.code, Some(2), "absent socket refuses");
    assert_eq!(transcript.frames.len(), 1);
}

#[test]
fn ssh_regular_file_refuses_kind_gate() {
    // S_ISSOCK required: a regular file at the canonical path
    // refuses (no read-probing, kind check only).
    let dir = tempfile::TempDir::new().expect("tempdir");
    let regular = dir.path().join("not-a-socket");
    std::fs::write(&regular, b"nope").expect("regular file writes");
    let regular = regular.to_string_lossy().into_owned();
    let argv_p = format!("--socket={regular}");
    let argv_c = format!("--socket-canonical={regular}");
    let transcript = run_guest(
        &[&argv_p, &argv_c],
        vec![
            hello_frame(),
            prepare_frame(serde_json::json!({"session_id": "s-kind"})),
        ],
    );
    assert_eq!(transcript.code, Some(2), "non-socket refuses");
    assert_eq!(transcript.frames.len(), 1);
}
