//! Sibling-relative guest discovery (task 1.1).
//!
//! Bare names resolve inside the directory; escapes, absences, and
//! non-executables refuse with the expected path named.

use std::os::unix::fs::PermissionsExt;

use cistella::framework::discovery::discover_in;

fn dir_with(name: &str, mode: u32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    dir
}

#[test]
fn executable_sibling_resolves() {
    let dir = dir_with("cistella-guest-probe", 0o755);
    let found = discover_in(dir.path(), "cistella-guest-probe").expect("must resolve");
    assert_eq!(found, dir.path().join("cistella-guest-probe"));
}

#[test]
fn missing_binary_names_expected_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = discover_in(dir.path(), "cistella-guest-absent").expect_err("absence must refuse");
    let message = error.to_string();
    assert!(
        message.contains("cistella-guest-absent"),
        "diagnostic names the binary, got: {message}"
    );
}

#[test]
fn non_executable_refuses() {
    let dir = dir_with("cistella-guest-noexec", 0o644);
    let error = discover_in(dir.path(), "cistella-guest-noexec").expect_err("non-exec must refuse");
    assert!(error.to_string().contains("not executable"), "got: {error}");
}

#[test]
fn directory_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("cistella-guest-dir")).expect("mkdir");
    let error = discover_in(dir.path(), "cistella-guest-dir").expect_err("directory must refuse");
    assert!(
        error.to_string().contains("not a regular file"),
        "got: {error}"
    );
}

#[test]
fn symlink_sibling_refused_unfollowed() {
    let dir = dir_with("cistella-guest-real", 0o755);
    std::os::unix::fs::symlink(
        dir.path().join("cistella-guest-real"),
        dir.path().join("cistella-guest-link"),
    )
    .expect("symlink");
    let error = discover_in(dir.path(), "cistella-guest-link").expect_err("symlink must refuse");
    assert!(
        error.to_string().contains("not a regular file"),
        "no-follow posture, got: {error}"
    );
}

#[test]
fn escape_names_refuse() {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["", ".", "..", "sub/guest", "../guest", "/abs/guest"] {
        let error = discover_in(dir.path(), name).expect_err("escape must refuse");
        assert!(
            error.to_string().contains("bare file name"),
            "name {name:?} got: {error}"
        );
    }
}
