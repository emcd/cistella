//! Digest-registry unit tests: admission, path shape, digest binding.
//!
//! All cases exercise the public registry surface (`resolve`)
//! against scratch registry roots — never the real install
//! directory.

use std::io::Write;

use cistella::framework::registry::{SHIPPED_REGISTRY_ID, resolve};

/// Writes `contents` to `name` under a scratch dir; returns the dir.
fn scratch_registry(name: &str, contents: &[u8]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    let mut file = std::fs::File::create(dir.path().join(name)).expect("registry file");
    file.write_all(contents).expect("registry bytes");
    file.sync_all().expect("registry sync");
    dir
}

#[test]
fn unknown_registry_refuses() {
    let dir = scratch_registry("wrap", b"bytes");
    let error = resolve(dir.path(), "remote", "wrap", &"0".repeat(64)).unwrap_err();
    assert!(error.to_string().contains("unknown artifact registry"));
}

#[test]
fn escaping_paths_refuse() {
    let dir = scratch_registry("wrap", b"bytes");
    for bad in ["", "/absolute/path", "../escape", "sub/../../escape", "./"] {
        let error = resolve(dir.path(), SHIPPED_REGISTRY_ID, bad, &"0".repeat(64)).unwrap_err();
        assert!(
            error.to_string().contains("artifact path"),
            "path {bad:?} got: {error}"
        );
    }
}

#[test]
fn digest_mismatch_refuses() {
    let dir = scratch_registry("wrap", b"actual-bytes");
    let error = resolve(dir.path(), SHIPPED_REGISTRY_ID, "wrap", &"0".repeat(64)).unwrap_err();
    assert!(error.to_string().contains("digest mismatch"));
}

#[test]
fn matching_digest_resolves_bytes() {
    // Empty file: SHA-256 of nothing.
    let dir = scratch_registry("wrap", b"");
    let pinned = resolve(
        dir.path(),
        SHIPPED_REGISTRY_ID,
        "wrap",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    )
    .unwrap();
    assert!(pinned.bytes.is_empty());
    assert_eq!(pinned.registry, SHIPPED_REGISTRY_ID);
    assert_eq!(pinned.path, "wrap");
}

#[test]
fn oversized_registry_file_refuses_without_reading() {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    let file = std::fs::File::create(dir.path().join("huge")).expect("registry file");
    file.set_len(17 * 1024 * 1024).expect("sparse size");
    drop(file);
    let error = resolve(dir.path(), SHIPPED_REGISTRY_ID, "huge", &"0".repeat(64)).unwrap_err();
    assert!(error.to_string().contains("exceeds registry read ceiling"));
}

#[test]
fn missing_registry_file_surfaces_io() {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    let error = resolve(dir.path(), SHIPPED_REGISTRY_ID, "absent", &"0".repeat(64)).unwrap_err();
    // Operational miss, not a contract refusal: the admitted shape
    // parsed, the file is simply not there.
    assert!(!error.to_string().contains("unknown artifact registry"));
    assert!(!error.to_string().contains("digest mismatch"));
}
