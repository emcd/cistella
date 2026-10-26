//! Operation-log pins: writer lifecycle, envelope, and shared fixtures.
//!
//! Split from the monolithic pins file at the file-size limit;
//! reader, gc-integration, and prune pins live beside it.

use cistella::operation_log::{
    Budget, Injected, Operation, Outcome, encode_frame, mint_op_id, op_id_valid, read_operation,
};

pub(crate) fn state_root() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("tmpdir")
}

pub(crate) fn start_only_frame() -> Vec<u8> {
    let payload = serde_json::json!({"seq": 0u64, "kind": "start"}).to_string();
    encode_frame(payload.as_bytes()).expect("encode")
}

pub(crate) fn oversize_frame(total_len: usize, seq: u64, kind: &str) -> Vec<u8> {
    let mut payload = serde_json::json!({"seq": seq, "kind": kind}).to_string();
    while payload.len() < total_len {
        payload.push(' ');
    }
    assert!(payload.len() >= total_len, "padding reached");
    let mut out = (payload.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(payload.as_bytes());
    out
}

pub(crate) fn craft_incomplete_op(root: &std::path::Path, name: &str, pid: u32, starttime: u64) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let start = serde_json::json!({
        "seq": 0u64,
        "kind": "start",
        "pid": pid,
        "process_start": starttime,
    })
    .to_string();
    let frame = encode_frame(start.as_bytes()).expect("encode");
    std::fs::write(dir.join("records.log"), frame).expect("write");
}
#[test]
fn minted_ids_are_path_safe() {
    for _ in 0..16 {
        let id = mint_op_id();
        assert!(op_id_valid(&id), "minted id is valid: {id}");
    }
    assert!(!op_id_valid(""), "empty rejects");
    assert!(!op_id_valid("../evil"), "slash rejects");
    assert!(!op_id_valid("UPPER"), "uppercase rejects");
    assert!(!op_id_valid(&"a".repeat(65)), "overlong rejects");
}

#[test]
fn writer_open_append_close_reads_completed() {
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open writes start record");
    let dir = op.dir().to_path_buf();
    let mut op = op;
    op.append(&serde_json::json!({"observed": "present"}))
        .expect("append body");
    op.close("success");
    let read = read_operation(&dir).expect("read");
    assert_eq!(read.discards, 0, "clean log discards nothing");
    assert_eq!(read.records.len(), 3, "start, body, terminal");
    assert!(
        matches!(read.outcome, Outcome::Completed(ref s) if s == "success"),
        "terminal outcome reads completed, got: {:?}",
        read.outcome
    );
}

#[test]
fn incomplete_operation_reads_unknown() {
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open writes start record");
    let dir = op.dir().to_path_buf();
    std::mem::forget(op);
    let read = read_operation(&dir).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Unknown),
        "missing terminal reads UNKNOWN, never success"
    );
}

#[test]
fn finalization_seam_creates_and_removes_the_marker() {
    // The seam itself, driven directly: begin creates the marker,
    // end renames it to finalized evidence. Wiring (that close
    // drives the seam) is pinned separately below.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    assert!(op.begin_finalization(), "marker created");
    assert!(dir.join(".finalizing").is_file(), "marker present");
    op.end_finalization();
    assert!(!dir.join(".finalizing").exists(), "marker removed by end");
    op.close("success");
    assert!(
        !dir.join(".finalizing").exists(),
        "close leaves no in-progress marker"
    );
    let read = read_operation(&dir).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Completed(ref s) if s == "success"),
        "terminal readable after finalization"
    );
}

#[test]
fn close_leaves_finalized_evidence_itself() {
    // The wiring pin: a fresh operation that nothing touched goes
    // through close alone, and the finalized evidence it carries
    // afterwards can only have come from close driving the seam -
    // the pin creates no evidence of its own. Removing close's
    // begin call, its end call, or both leaves no evidence behind
    // and fails this pin.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    op.close("success");
    assert!(
        !dir.join(".finalizing").exists(),
        "no in-progress marker left behind"
    );
    assert!(
        dir.join(".finalized").is_file(),
        "close itself produced finalized evidence"
    );
    let read = read_operation(&dir).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Completed(ref s) if s == "success"),
        "terminal readable after finalization"
    );
    let terminal = read.records.last().expect("terminal record");
    assert_eq!(
        terminal.get("finalization").and_then(|v| v.as_str()),
        Some("locked"),
        "uncontended close records a locked finalization"
    );
}

#[test]
fn cap_exhaustion_stops_the_stream() {
    let root = state_root();
    let log_root = root.path().join("logs");
    let mut op = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open writes start record");
    let dir = op.dir().to_path_buf();
    // Fill the per-operation cap with maximum frames on the real
    // writer seam: the refusing append stops the stream, and no
    // terminal may follow the torn accounting.
    let chunk = vec![b'x'; cistella::operation_log::MAX_FRAME_PAYLOAD - 64];
    let chunk_str = String::from_utf8(chunk).expect("ascii");
    let mut refused = false;
    for _ in 0..8 {
        if op.append(&serde_json::json!({"pad": chunk_str})).is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused, "the cap refuses on the real seam");
    assert!(
        op.append(&serde_json::json!({"n": 1})).is_err(),
        "no frames behind a stopped stream"
    );
    op.close("success");
    let read = read_operation(&dir).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Unknown),
        "stopped stream never reads completed"
    );
}

#[test]
fn unknown_verb_maps_to_fixed_label() {
    // The fixed vocabulary holds in release builds too: an unknown
    // verb is recorded under the fixed `unknown` label, never
    // verbatim.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open_with(
        &log_root,
        "bogus",
        None,
        &Injected::default(),
        &Budget {
            total: 64 * 1024 * 1024,
            reserve: 0,
            success_cap: 2000,
            age_secs: 3600,
        },
    )
    .expect("unknown verb still opens");
    let dir = op.dir().to_path_buf();
    op.close("success");
    let read = read_operation(&dir).expect("read");
    let start = &read.records[0];
    assert_eq!(
        start.get("actor").and_then(|v| v.as_str()),
        Some("unknown"),
        "unknown verb maps to the fixed label"
    );
}

#[test]
fn envelope_carries_role_and_end_timestamp() {
    // The envelope names the actor's role from the fixed
    // vocabulary, and the terminal record carries the end wall
    // clock beside the monotonic duration.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    op.close("success");
    let read = read_operation(&dir).expect("read");
    let start = &read.records[0];
    assert_eq!(
        start.get("actor").and_then(|v| v.as_str()),
        Some("gc"),
        "actor names the verb"
    );
    assert_eq!(
        start.get("role").and_then(|v| v.as_str()),
        Some("orphan-reaping"),
        "role comes from the fixed vocabulary"
    );
    let terminal = read.records.last().expect("terminal");
    assert!(
        terminal.get("end_wall").and_then(|v| v.as_u64()).is_some(),
        "terminal carries the end wall clock"
    );
    assert!(
        terminal
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .is_some(),
        "terminal carries the monotonic duration"
    );
}

#[test]
fn child_operation_records_its_parent() {
    // Nested operations link to their parent by id.
    let root = state_root();
    let log_root = root.path().join("logs");
    let parent = Operation::open(&log_root, "gc", None, &Injected::default()).expect("parent");
    let parent_id = parent
        .dir()
        .file_name()
        .expect("name")
        .to_string_lossy()
        .to_string();
    parent.close("success");
    let child = Operation::open(
        &log_root,
        "teardown",
        Some(&parent_id),
        &Injected::default(),
    )
    .expect("child");
    let dir = child.dir().to_path_buf();
    child.close("success");
    let read = read_operation(&dir).expect("read");
    assert_eq!(
        read.records[0].get("parent").and_then(|v| v.as_str()),
        Some(parent_id.as_str()),
        "child links its parent"
    );
}

#[test]
fn close_records_a_locked_finalization() {
    // The finalization hold is observable: a normal close records
    // a locked finalization in its terminal, so dropping the
    // lock acquisition would flip this field and fail the pin.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    op.close("success");
    let read = read_operation(&dir).expect("read");
    let terminal = read.records.last().expect("terminal");
    assert_eq!(
        terminal.get("finalization").and_then(|v| v.as_str()),
        Some("locked"),
        "close held the admission lock across finalization"
    );
}
