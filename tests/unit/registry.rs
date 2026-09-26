//! Digest-registry unit tests: exact admission, no-symlink,
//! FD-pinned size, digest binding.
//!
//! All cases exercise the public registry surface (`resolve`,
//! `digest_sibling`) against scratch registry roots — never the
//! real install directory.

use std::io::Write;

use cistella::framework::registry::{
    SHIPPED_REGISTRY_ID, WRAPPER_FILE_NAME, digest_sibling, resolve,
};

/// Writes `contents` to the admitted wrapper name under a scratch
/// dir; returns the dir.
fn scratch_registry(contents: &[u8]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    let mut file =
        std::fs::File::create(dir.path().join(WRAPPER_FILE_NAME)).expect("registry file");
    file.write_all(contents).expect("registry bytes");
    file.sync_all().expect("registry sync");
    dir
}

#[test]
fn unknown_registry_refuses() {
    let dir = scratch_registry(b"bytes");
    let error = resolve(dir.path(), "remote", WRAPPER_FILE_NAME, &"0".repeat(64)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only the shipped wrapper is admitted"),
        "got: {error}"
    );
}

#[test]
fn unadmitted_path_refuses_despite_existing_file() {
    let dir = scratch_registry(b"bytes");
    // A second readable file under the root is NOT admitted: the
    // extension selects among table entries, never names files.
    std::fs::write(dir.path().join("other-binary"), b"bytes").expect("other file");
    let error = resolve(
        dir.path(),
        SHIPPED_REGISTRY_ID,
        "other-binary",
        &"0".repeat(64),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only the shipped wrapper is admitted"),
        "got: {error}"
    );
}

#[test]
fn escaping_paths_refuse_as_unadmitted() {
    let dir = scratch_registry(b"bytes");
    // Only the exact admitted pair passes admission; everything
    // else refuses before any filesystem touch.
    for bad in ["", "/absolute/path", "../escape", "sub/../../escape"] {
        let error = resolve(dir.path(), SHIPPED_REGISTRY_ID, bad, &"0".repeat(64)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only the shipped wrapper is admitted"),
            "path {bad:?} got: {error}"
        );
    }
    let error = resolve(dir.path(), "remote", WRAPPER_FILE_NAME, &"0".repeat(64)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only the shipped wrapper is admitted"),
        "got: {error}"
    );
}

#[test]
fn symlink_at_admitted_name_refuses_without_following() {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    // Outside target with known bytes: if the open followed the
    // link, the digest below would MATCH and resolve would succeed.
    let outside = dir.path().join("outside-target");
    std::fs::write(&outside, b"").expect("outside file");
    std::os::unix::fs::symlink(&outside, dir.path().join(WRAPPER_FILE_NAME)).expect("symlink");
    let error = resolve(
        dir.path(),
        SHIPPED_REGISTRY_ID,
        WRAPPER_FILE_NAME,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("must not be a symlink"),
        "got: {error}"
    );
}

#[test]
fn digest_mismatch_refuses() {
    let dir = scratch_registry(b"actual-bytes");
    let error = resolve(
        dir.path(),
        SHIPPED_REGISTRY_ID,
        WRAPPER_FILE_NAME,
        &"0".repeat(64),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("digest mismatch"),
        "got: {error}"
    );
}

#[test]
fn matching_digest_resolves_bytes() {
    // Empty file: SHA-256 of nothing.
    let dir = scratch_registry(b"");
    let pinned = resolve(
        dir.path(),
        SHIPPED_REGISTRY_ID,
        WRAPPER_FILE_NAME,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    )
    .unwrap();
    assert!(pinned.bytes.is_empty());
    assert_eq!(pinned.registry, SHIPPED_REGISTRY_ID);
    assert_eq!(pinned.path, WRAPPER_FILE_NAME);
}

#[test]
fn oversized_registry_file_refuses_without_reading() {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    let admitted = dir.path().join(WRAPPER_FILE_NAME);
    let file = std::fs::File::create(&admitted).expect("registry file");
    file.set_len(17 * 1024 * 1024).expect("sparse size");
    drop(file);
    let error = resolve(
        dir.path(),
        SHIPPED_REGISTRY_ID,
        WRAPPER_FILE_NAME,
        &"0".repeat(64),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("exceeds registry read ceiling"),
        "got: {error}"
    );
}

#[test]
fn missing_registry_file_surfaces_io() {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    let error = resolve(
        dir.path(),
        SHIPPED_REGISTRY_ID,
        WRAPPER_FILE_NAME,
        &"0".repeat(64),
    )
    .unwrap_err();
    // Operational miss, not a refusal: admission passed, the digest
    // parsed, the file is simply not there.
    assert!(
        matches!(error, cistella::error::CistellaError::Io(_)),
        "got: {error}"
    );
}

#[test]
fn digest_sibling_refuses_symlink_closed() {
    let dir = tempfile::tempdir().expect("scratch registry dir");
    let outside = dir.path().join("outside-target");
    std::fs::write(&outside, b"bytes").expect("outside file");
    std::os::unix::fs::symlink(&outside, dir.path().join(WRAPPER_FILE_NAME)).expect("symlink");
    let error = digest_sibling(dir.path(), WRAPPER_FILE_NAME).unwrap_err();
    assert!(
        matches!(error, cistella::error::CistellaError::Io(_)),
        "got: {error}"
    );
}
