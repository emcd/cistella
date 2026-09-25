//! Podman namespace probe — Landlock survival across the userns/exec boundary.
//!
//! Answers the question `landlock_spike.rs` defers: do Landlock rules
//! established by the helper survive across `podman run
//! --userns=keep-id`'s user-namespace and exec boundary into the
//! wrapped process? Hypothesis (now verified): yes — Landlock is
//! enforced per-task at the kernel level, not per-namespace, so the
//! restriction follows the thread through the container runtime's
//! fork/exec chain and the helper's own `execvp`.
//!
//! Shape: one-shot `podman run --rm` invocations (no detached
//! container, no GC residue) with the host-built helper bind-mounted
//! read-only at `/landlock_helper`. The wrapped argv runs entirely
//! inside the container's user namespace.
//!
//! Two assertions, mirroring the host-side spike:
//!   1. `admitted` — wrapped `sh` echoes a marker; exit 0 proves the
//!      ruleset reached the wrapped process inside the container.
//!   2. `denied` — wrapped `cat /etc/hostname` with `/etc` outside
//!      the allow list; non-zero exit plus an access-denied diagnostic
//!      proves the denial is enforced inside the container.
//!
//! Live-tier gating: both tests are `#[ignore]`-d. They need podman,
//! the example image, and a host-built helper whose loader the image
//! satisfies (verified: glibc 2.39 host binary runs on the
//! `opencode:example` image). Missing podman or image skips with a
//! diagnostic instead of failing — same convention as
//! `transport.rs`.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The helper's mount point inside the probe container.
const HELPER_MOUNT: &str = "/landlock_helper";

fn podman_available() -> bool {
    Command::new("podman")
        .args(["info", "--format", "{{.Host.Security.Rootless}}"])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn image_ref() -> String {
    std::env::var("CISTELLA_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/cistella/opencode:example".to_string())
}

/// Resolves the host-built helper. Same mtime-descending logic as
/// `resolve_example` in `landlock_spike.rs`; duplicated rather than
/// shared so the tier-2-approved spike file stays untouched (the
/// spike file itself notes the sharing is out of scope).
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

/// Runs the helper inside a one-shot `--userns=keep-id` container and
/// returns `(exit_code, stdout, stderr)`. Callers that reach this far
/// have podman and the image; a `podman run` failure here is a real
/// failure, not a skip.
fn run_helper_in_container(allows: &[&str], wrapped_argv: &[&str]) -> (i32, String, String) {
    let helper = landlock_helper_path();
    let image = image_ref();
    let mount = format!("{}:{HELPER_MOUNT}:ro", helper.display());
    let mut command = Command::new("podman");
    command.args([
        "run",
        "--rm",
        "--userns=keep-id",
        "-v",
        &mount,
        "--",
        &image,
        HELPER_MOUNT,
    ]);
    for allow in allows {
        command.arg(format!("--allow={allow}"));
    }
    command.arg("--");
    for arg in wrapped_argv {
        command.arg(arg);
    }
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("podman run must spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.code().unwrap_or(-1), stdout, stderr)
}

/// Guards both probe tests: skip with a diagnostic when the seat
/// cannot run the probe (no podman, or the image is absent).
fn probe_preconditions() -> bool {
    if !podman_available() {
        eprintln!("skip: podman not available");
        return false;
    }
    let image = image_ref();
    let has_image = Command::new("podman")
        .args(["image", "exists", &image])
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_image {
        eprintln!("skip: image {image} not present (build via data/dockerfiles/validate.sh)");
        return false;
    }
    true
}

/// `admitted` across the boundary — the helper applies the ruleset
/// inside the container's user namespace, execs `sh`, and `sh`
/// echoes the marker. Exit 0 plus the marker on stdout proves the
/// Landlock restriction reached the wrapped process through both
/// the runtime's fork/exec chain and the helper's `execvp`.
#[ignore = "live: requires podman, example image, loader-compatible helper"]
#[test]
fn admitted_inside_keep_id_container() {
    if !probe_preconditions() {
        return;
    }
    const MARKER: &str = "PODMAN_ANCESTRY_ADMITTED";
    let wrapped = ["/bin/sh", "-c", &format!("echo {MARKER}")];
    // Same directory-only allow shape as the host spike: `/usr`
    // covers the shell binary, loader, and libc; `/etc` covers the
    // ld cache directory; `/proc/self/fd` covers stdio; `/tmp` is
    // unused here but keeps the shape identical.
    let allows = ["/tmp", "/proc/self/fd", "/usr", "/etc"];
    let (exit_code, stdout, stderr) = run_helper_in_container(&allows, &wrapped);
    assert_eq!(
        exit_code, 0,
        "wrapped shell must exit 0 inside keep-id container; stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert!(
        stdout.contains(MARKER),
        "marker must reach container stdout through the Landlock-wrapped exec; stdout: {stdout:?}"
    );
}

/// `denied` across the boundary — `/etc` is outside the allow list,
/// so `cat /etc/hostname` inside the container must fail with an
/// access-denied diagnostic. Proves the denial (not just the
/// admission) is enforced on the wrapped process inside the
/// container's user namespace.
#[ignore = "live: requires podman, example image, loader-compatible helper"]
#[test]
fn denied_inside_keep_id_container() {
    if !probe_preconditions() {
        return;
    }
    let wrapped = ["/bin/sh", "-c", "cat /etc/hostname"];
    let allows = ["/usr", "/proc/self/fd"];
    let (exit_code, stdout, stderr) = run_helper_in_container(&allows, &wrapped);
    assert_ne!(
        exit_code, 0,
        "wrapped cat of a denied path must fail inside keep-id container; stdout: {stdout:?}"
    );
    let combined = format!("{stdout} {stderr}").to_lowercase();
    assert!(
        combined.contains("permission denied")
            || combined.contains("eacces")
            || combined.contains("access denied"),
        "denied path must surface as access-denied inside the container, got stdout: {stdout:?} stderr: {stderr:?}"
    );
}
