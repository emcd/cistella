//! Operation-log pins: framing and the reader contract.
//!
//! Split from `operation_log.rs` at the file-size limit; shared
//! fixtures live there.

use super::operation_log::{oversize_frame, start_only_frame, state_root};
use cistella::operation_log::{Outcome, decode_frames, encode_frame, read_operation};
#[test]
fn frame_round_trip_preserves_sequence() {
    let first = encode_frame(br#"{"seq":0,"kind":"start"}"#).expect("encode");
    let second = encode_frame(br#"{"seq":1,"kind":"body"}"#).expect("encode");
    let mut bytes = first;
    bytes.extend_from_slice(&second);
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(discards, 0, "clean stream discards nothing");
    assert_eq!(records.len(), 2, "both frames read");
}

#[test]
fn oversize_payload_refuses_before_allocation() {
    let big = vec![0u8; cistella::operation_log::MAX_FRAME_PAYLOAD + 1];
    assert!(
        encode_frame(&big).is_err(),
        "oversize frame refuses at encode"
    );
    let mut stream =
        u32::to_be_bytes((cistella::operation_log::MAX_FRAME_PAYLOAD + 1) as u32).to_vec();
    stream.extend_from_slice(&[0u8; 8]);
    let (records, discards) = decode_frames(&stream);
    assert!(records.is_empty(), "oversize claim reads nothing");
    assert_eq!(discards, 1, "oversize claim discards once");
}

#[test]
fn torn_tail_discards_and_keeps_prefix() {
    let mut bytes = start_only_frame();
    let partial = encode_frame(br#"{"seq":1}"#).expect("encode");
    bytes.extend_from_slice(&partial[..3]);
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(records.len(), 1, "complete prefix preserved");
    assert_eq!(discards, 1, "torn frame discarded and counted");
    let (records, discards) = decode_frames(&[0x00]);
    assert!(records.is_empty(), "partial prefix reads nothing");
    assert_eq!(discards, 1, "partial prefix counted");
}

#[test]
fn sequence_gap_and_bad_json_stop_the_stream() {
    let first = start_only_frame();
    let gap_payload = serde_json::json!({"seq": 5u64, "kind": "body"}).to_string();
    let mut bytes = first;
    bytes.extend_from_slice(&encode_frame(gap_payload.as_bytes()).expect("encode"));
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(records.len(), 1, "gap discards from the gap on");
    assert_eq!(discards, 1, "gap counted");
    let bad = encode_frame(b"not json").expect("encode");
    let (records, discards) = decode_frames(&bad);
    assert!(records.is_empty(), "invalid JSON reads nothing");
    assert_eq!(discards, 1, "invalid JSON counted");
}

#[test]
fn missing_start_reads_unknown_with_prefix_kept() {
    let root = state_root();
    let dir = root.path().join("abc123");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let body = encode_frame(br#"{"seq":0,"kind":"body"}"#).expect("encode");
    std::fs::write(dir.join("records.log"), body).expect("write");
    let read = read_operation(&dir).expect("read");
    assert!(
        read.records.is_empty(),
        "a body first violates the schema; nothing is kept"
    );
    assert_eq!(read.discards, 1, "schema violation counted");
    assert!(
        matches!(read.outcome, Outcome::Unknown),
        "missing start reads UNKNOWN"
    );
}

#[test]
fn oversize_prefix_with_full_payload_reads_nothing() {
    // The prefix declares more than the maximum while the file
    // actually holds that many bytes: only the max-payload check
    // can reject it (the remaining-bytes check passes).
    let frame = oversize_frame(cistella::operation_log::MAX_FRAME_PAYLOAD + 1, 0, "start");
    let (records, discards) = decode_frames(&frame);
    assert!(records.is_empty(), "oversize reads nothing");
    assert_eq!(discards, 1, "oversize counted once");
}

#[test]
fn short_payload_with_full_prefix_is_torn() {
    // Complete 4-byte prefix declaring more than follows: only the
    // remaining-bytes check catches it.
    let mut stream = 100u32.to_be_bytes().to_vec();
    stream.extend_from_slice(&[0u8; 10]);
    let (records, discards) = decode_frames(&stream);
    assert!(records.is_empty(), "short payload reads nothing");
    assert_eq!(discards, 1, "short payload counted once");
}

#[test]
fn invalid_json_stops_the_stream() {
    // A corrupt frame between two valid ones: the stream keeps the
    // valid prefix and stops, rather than skipping the bad frame.
    let first = encode_frame(br#"{"seq":0,"kind":"start"}"#).expect("encode");
    let bad = encode_frame(b"not json").expect("encode");
    let third = encode_frame(br#"{"seq":1,"kind":"body"}"#).expect("encode");
    let mut bytes = first;
    bytes.extend_from_slice(&bad);
    bytes.extend_from_slice(&third);
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(records.len(), 1, "only the valid prefix survives");
    assert_eq!(discards, 1, "corrupt frame counted, stream stops");
}

#[test]
fn terminal_without_start_reads_unknown() {
    // A terminal-shaped record with no start: complete and
    // well-formed, but the outcome reads UNKNOWN, never completed.
    let root = state_root();
    let dir = root.path().join("abc123");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let terminal = encode_frame(br#"{"seq":0,"kind":"terminal","outcome":"x"}"#).expect("encode");
    let (records, discards) = decode_frames(&terminal);
    assert!(
        records.is_empty(),
        "terminal without start is schema-invalid"
    );
    assert_eq!(discards, 1, "schema violation counted");
    std::fs::write(dir.join("records.log"), terminal).expect("write");
    let read = read_operation(&dir).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Unknown),
        "terminal without start reads UNKNOWN"
    );
}

#[test]
fn body_outcome_without_terminal_reads_unknown() {
    // A body record carrying an outcome field must not satisfy the
    // terminal check: only a terminal record completes.
    let root = state_root();
    let dir = root.path().join("abc123");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let start = encode_frame(br#"{"seq":0,"kind":"start"}"#).expect("encode");
    let body = encode_frame(br#"{"seq":1,"kind":"body","outcome":"success"}"#).expect("encode");
    let mut bytes = start;
    bytes.extend_from_slice(&body);
    std::fs::write(dir.join("records.log"), bytes).expect("write");
    let read = read_operation(&dir).expect("read");
    assert_eq!(read.records.len(), 2, "both records kept");
    assert!(
        matches!(read.outcome, Outcome::Unknown),
        "a body outcome field is not a terminal"
    );
}

#[test]
fn frames_after_terminal_end_the_prefix() {
    // A terminal closes the stream: trailing bytes are corruption,
    // not records, even when they would parse on their own.
    let start = encode_frame(br#"{"seq":0,"kind":"start"}"#).expect("encode");
    let terminal = encode_frame(br#"{"seq":1,"kind":"terminal","outcome":"ok"}"#).expect("encode");
    let trailing = encode_frame(br#"{"seq":2,"kind":"body"}"#).expect("encode");
    let mut bytes = start;
    bytes.extend_from_slice(&terminal);
    bytes.extend_from_slice(&trailing);
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(records.len(), 2, "complete prefix kept");
    assert_eq!(discards, 1, "trailing bytes counted once");
}

#[test]
fn duplicate_start_and_unknown_kind_end_the_prefix() {
    let start = encode_frame(br#"{"seq":0,"kind":"start"}"#).expect("encode");
    let restart = encode_frame(br#"{"seq":1,"kind":"start"}"#).expect("encode");
    let mut bytes = start;
    bytes.extend_from_slice(&restart);
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(records.len(), 1, "second start rejected");
    assert_eq!(discards, 1, "rejected start counted");
    let start = encode_frame(br#"{"seq":0,"kind":"start"}"#).expect("encode");
    let bogus = encode_frame(br#"{"seq":1,"kind":"teardown"}"#).expect("encode");
    let mut bytes = start;
    bytes.extend_from_slice(&bogus);
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(records.len(), 1, "unknown kind rejected");
    assert_eq!(discards, 1, "unknown kind counted");
    let start = encode_frame(br#"{"seq":0,"kind":"start"}"#).expect("encode");
    let noseq = encode_frame(br#"{"kind":"body"}"#).expect("encode");
    let mut bytes = start;
    bytes.extend_from_slice(&noseq);
    let (records, discards) = decode_frames(&bytes);
    assert_eq!(records.len(), 1, "missing seq rejected");
    assert_eq!(discards, 1, "missing seq counted");
}

#[test]
fn truncate_keeps_utf8_boundaries() {
    // 127 ASCII bytes plus e-acute: byte 128 lands inside the
    // multi-byte character, and naive slicing would panic.
    let mut raw = "x".repeat(127);
    raw.push('\u{e9}');
    assert_eq!(raw.len(), 129);
    let out = cistella::operation_log::truncate_str(&raw, 128);
    assert!(
        out.is_char_boundary(out.len() - "[truncated]".len()),
        "no torn char"
    );
    assert!(out.ends_with("[truncated]"), "marked");
    assert!(
        out.len() - "[truncated]".len() < raw.len(),
        "content shortened"
    );
}

#[test]
fn injected_claims_are_bounded_and_validated() {
    let mut values = std::collections::HashMap::new();
    let mut evil = "y".repeat(127);
    evil.push('\u{e9}');
    values.insert("CISTELLA_TEST_RUN_ID".to_string(), evil);
    values.insert("CISTELLA_PARENT_OP_ID".to_string(), "../evil".to_string());
    values.insert("CISTELLA_TEST_NAME".to_string(), "ok".to_string());
    let injected = cistella::operation_log::read_injected_from(&|key| values.get(key).cloned());
    let run_id = injected.run_id.expect("run id kept");
    assert!(
        run_id.is_char_boundary(run_id.len() - "[truncated]".len()),
        "valid UTF-8"
    );
    assert!(injected.parent_op_id.is_none(), "path injection refused");
    assert_eq!(injected.test_name.as_deref(), Some("ok"));
}

#[test]
fn oversize_file_refuses_instead_of_reading() {
    // A file larger than the read bound is refused outright rather
    // than allocated against: the bound is enforced by the read
    // itself, not by a metadata pre-check.
    let root = state_root();
    let dir = root.path().join("abc123");
    std::fs::create_dir_all(&dir).expect("mkdir");
    // Above the read bound (per-op cap plus max frame plus slack).
    let big = vec![0x7au8; 2 * 1024 * 1024];
    std::fs::write(dir.join("records.log"), &big).expect("write");
    assert!(
        read_operation(&dir).is_err(),
        "oversize file is refused, not read"
    );
}
