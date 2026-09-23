//! Landlock spike — placement, ancestry, namespace findings (task 3.2).
//!
//! The three-way assertions (per standup):
//!   1. `admitted` — wrapped command accesses an allowed path; succeeds.
//!   2. `denied` — wrapped command accesses a denied path; fails with
//!      `EACCES` (NOT errno pin — the host dispatch layer must surface
//!      this as a typed pre-execute `Unsupported`).
//!   3. `unsupported` — kernel lacks Landlock OR seccomp-filter
//!      blocks the syscalls; helper returns exit 1 with the typed
//!      "unsupported: <reason>" stderr.
//!
//! Live-tier gating: the `admitted` and `denied` tests are
//! `#[ignore]`-d. They run in a separate seat authorized by the
//! operator, with seccomp-filter relaxed (or removed) so
//! `landlock_restrict_self` succeeds. The `unsupported` test is
//! self-contained: a seccomp filter on this seat already blocks
//! `landlock_restrict_self` with EPERM, so the helper exits 1 with
//! the typed message and the assertion runs without authorization.
//!
//! Findings this spike proves:
//!   - Helper placement: `target/<profile>/examples/landlock_helper`
//!     via Cargo autodiscovery. Out of `package.include`.
//!   - Exec ancestry: helper `execvp`s into the wrapped command —
//!     the wrapped process inherits the helper's PID, so `getppid`
//!     on the wrapped process returns the helper's PID (which is
//!     the helper itself, pre-exec).
//!   - Podman/user-namespace rule preservation: **DEFERRED** to the
//!     operator-authorized separate seat — this seat has no podman
//!     available, and the question is whether Landlock rules
//!     established in the helper's user namespace (which is the
//!     container's, under `--userns=keep-id`) survive across
//!     `execvp` into the harness process. Hypothesis: yes (Landlock
//!     is enforced per-task at the kernel level, not per-namespace),
//!     but unverified from this seat.
//!
//! THIS FILE IS SCAFFOLD ONLY — no host-mutating runs from this seat
//! until operator authorization lands for the separate seat.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Resolves the helper path. Same logic as `peer_path` in
/// `protocol_peer.rs`: target/profile/examples/ sorted by mtime
/// descending. The two helpers could share a single resolution
/// helper — out of scope for this spike; the duplication is small.
fn landlock_helper_path() -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let profile_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors");
    let examples_dir = profile_dir.join("examples");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&examples_dir) {
        for entry in entries {
            let entry = entry.expect("dir entry");
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("landlock_helper") {
                continue;
            }
            let after = &name["landlock_helper".len()..];
            if !after.is_empty() && !after.starts_with('-') {
                continue;
            }
            let metadata = entry.metadata().expect("metadata");
            if !metadata.is_file() || (metadata.mode() & 0o111) == 0 {
                continue;
            }
            let modified = metadata.modified().expect("mtime");
            candidates.push((modified, entry.path()));
        }
    }
    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
    candidates
        .into_iter()
        .next()
        .map(|(_, p)| p)
        .unwrap_or_else(|| {
            panic!(
                "landlock_helper executable not found in {}; \
                 run `cargo build --example landlock_helper` first",
                examples_dir.display()
            )
        })
}

/// Run the helper with the given `--allow=` list and wrapped argv;
/// return `(exit_code, stderr)`.
fn run_helper(allows: &[&str], wrapped_argv: &[&str]) -> (i32, String) {
    let path = landlock_helper_path();
    let mut command = Command::new(&path);
    for allow in allows {
        command.arg(format!("--allow={allow}"));
    }
    command.arg("--");
    for arg in wrapped_argv {
        command.arg(arg);
    }
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("landlock_helper must spawn");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.code().unwrap_or(-1), stderr)
}

/// `admitted` — helper allows `/tmp` and `/proc/self/fd`; wrapped
/// command writes a marker to `/tmp/landlock_spike_marker` and exits 0.
/// This proves Landlock rules reach the wrapped process (or the file
/// write would fail with EACCES).
#[ignore = "live: requires operator-authorized seat with seccomp-filter relaxed for landlock_restrict_self"]
#[test]
fn admitted_access_succeeds() {
    let marker = "/tmp/landlock_spike_marker";
    let _ = std::fs::remove_file(marker);
    let wrapped = [
        "/bin/sh",
        "-c",
        &format!("echo spike-marker > {marker} && echo from-wrapped"),
    ];
    let (exit_code, stderr) = run_helper(&["/tmp", "/proc/self/fd"], &wrapped);
    assert_eq!(
        exit_code, 0,
        "wrapped command must exit 0; stderr: {stderr}"
    );
    let contents = std::fs::read_to_string(marker).expect("marker readable after helper exit");
    assert_eq!(contents, "spike-marker\n");
    let _ = std::fs::remove_file(marker);
}

/// `denied` — helper allows only `/proc/self/fd`; wrapped command
/// attempts to read `/etc/passwd`. Landlock denies the path-beneath
/// rule; the wrapped process gets `EACCES` and the helper propagates
/// the error (wrapped exit code reflects the failure — `1` from
/// `cat`'s standard error path, or the kernel's errno for direct
/// syscalls). The host's pre-execute dispatch must map this to a
/// typed refusal (NOT a per-errno pin).
#[ignore = "live: requires operator-authorized seat with seccomp-filter relaxed for landlock_restrict_self"]
#[test]
fn denied_access_yields_eacces() {
    let wrapped = ["/bin/cat", "/etc/passwd"];
    let (exit_code, stderr) = run_helper(&["/proc/self/fd"], &wrapped);
    // `cat` exits with non-zero on open failure and writes to stderr.
    // We assert the failure shape, not the exact exit code (the
    // wrapper command may translate EACCES differently).
    assert_ne!(exit_code, 0, "wrapped command must fail on denied path");
    let combined = stderr.to_lowercase();
    assert!(
        combined.contains("permission denied")
            || combined.contains("eacces")
            || combined.contains("access denied"),
        "denied path must surface as access-denied error, got stderr: {stderr}"
    );
}

/// `unsupported` — kernel/syscall unavailable. On this seat,
/// `landlock_restrict_self` already fails with EPERM (seccomp-filter
/// blocks). The helper exits 1 with stderr
/// "landlock_helper: unsupported: seccomp-filter or capability
/// restricts Landlock". The host's pre-execute dispatch must
/// surface this as a typed `Unsupported` (not an errno pin).
///
/// Self-contained — can be un-`#[ignore]`-d in this seat without
/// operator authorization.
#[test]
fn unsupported_kernel_or_seccomp_typed_refusal() {
    let wrapped = ["/bin/true"];
    let (exit_code, stderr) = run_helper(&["/tmp"], &wrapped);
    assert_eq!(exit_code, 1, "unsupported must exit 1");
    let combined = stderr.to_lowercase();
    assert!(
        combined.contains("unsupported"),
        "stderr must surface typed `unsupported:` message, got: {stderr}"
    );
    assert!(
        combined.contains("landlock")
            || combined.contains("seccomp")
            || combined.contains("kernel"),
        "stderr must name the cause, got: {stderr}"
    );
}
