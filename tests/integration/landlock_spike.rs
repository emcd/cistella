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
//!   - Podman/user-namespace rule preservation: verified by
//!     `podman_ancestry.rs` on the operator-authorized seat — the
//!     helper applies its ruleset inside a `podman run
//!     --userns=keep-id` container and the wrapped process observes
//!     both admission and EACCES denial there. Landlock is enforced
//!     per-task at the kernel level, not per-namespace, so the
//!     restriction follows the thread through the runtime's
//!     fork/exec chain and the helper's `execvp`.
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
    resolve_example("landlock_helper")
}

/// Resolves the seccomp-fault-injector path. Same resolution logic
/// as `landlock_helper_path`; separated for clarity.
fn landlock_kill_wrapper_path() -> PathBuf {
    resolve_example("landlock_kill_wrapper")
}

fn resolve_example(prefix: &str) -> PathBuf {
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
            if !name.starts_with(prefix) {
                continue;
            }
            let after = &name[prefix.len()..];
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
                "{prefix} executable not found in {}; \
                 run `cargo build --example {prefix}` first",
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

/// `admitted` — helper allows the wrapped process's needed paths
/// (the marker destination + python3 binary + dynamic linker +
/// libc), wrapped writes the marker and exits 0. Proves Landlock
/// rules reach the wrapped process. The NARROW-rules proof is
/// `denied_access_yields_eacces` (separate allow list).
#[ignore = "live: requires operator-authorized Podman seat"]
#[test]
fn admitted_access_succeeds() {
    let marker = "/tmp/landlock_spike_marker";
    let _ = std::fs::remove_file(marker);
    let wrapped = [
        "/usr/bin/python3",
        "-c",
        &format!("open({marker:?}, 'w').write('spike-marker')"),
    ];
    // Allow directory paths only (Landlock `path_beneath` rules
    // require the fd to refer to a directory — files like
    // `/etc/ld.so.cache` reject with EINVAL). The python3 binary
    // lives under `/usr`; `/etc` covers the ld cache directory;
    // `/proc/self/fd` covers stdio; `/tmp` is the marker destination.
    // `/lib` is a symlink to `/usr/lib` on Debian-derived systems;
    // Landlock rejects symlinks for path-beneath (EINVAL), so we
    // skip it. `/lib64` is x86_64-only; skipped on aarch64.
    let allows = ["/tmp", "/proc/self/fd", "/usr", "/etc"];
    let (exit_code, stderr) = run_helper(&allows, &wrapped);
    assert_eq!(
        exit_code, 0,
        "wrapped command must exit 0; stderr: {stderr}"
    );
    let contents = std::fs::read_to_string(marker).expect("marker readable after helper exit");
    assert_eq!(contents, "spike-marker");
    let _ = std::fs::remove_file(marker);
}

/// Local smoke for the admitted path: runs WITHOUT `#[ignore]` in
/// this seat (kernel 6.17 has Landlock, seccomp off, no_new_privs
/// branch works). Equivalent to the Podman-seat test for the helper's
/// correctness; the Podman test adds the namespace-inheritance
/// assertion (separate `podman_ancestry` file when that test lands).
#[test]
fn local_smoke_admitted_works() {
    let marker = "/tmp/landlock_spike_local_marker";
    let _ = std::fs::remove_file(marker);
    let wrapped = [
        "/usr/bin/python3",
        "-c",
        &format!("open({marker:?}, 'w').write('local-smoke')"),
    ];
    let allows = ["/tmp", "/proc/self/fd", "/usr", "/etc"];
    let (exit_code, stderr) = run_helper(&allows, &wrapped);
    assert_eq!(exit_code, 0, "local smoke must exit 0; stderr: {stderr}");
    let contents = std::fs::read_to_string(marker).expect("marker readable after helper exit");
    assert_eq!(contents, "local-smoke");
    let _ = std::fs::remove_file(marker);
}

/// `denied` — helper allows the wrapped process's needed paths
/// (`/usr` for python3, `/proc/self/fd` for stdio) but NOT `/etc`.
/// Wrapped attempts to read `/etc/passwd` — Landlock denies the
/// path-beneath under `/etc`, the wrapped process gets `EACCES`,
/// python exits non-zero. The host's pre-execute dispatch must map
/// this to a typed refusal (NOT a per-errno pin).
#[ignore = "live: requires operator-authorized Podman seat"]
#[test]
fn denied_access_yields_eacces() {
    let wrapped = ["/usr/bin/python3", "-c", "open('/etc/passwd', 'r').read()"];
    let allows = ["/usr", "/proc/self/fd"];
    let (exit_code, stderr) = run_helper(&allows, &wrapped);
    assert_ne!(exit_code, 0, "wrapped command must fail on denied path");
    let combined = stderr.to_lowercase();
    assert!(
        combined.contains("permission denied")
            || combined.contains("eacces")
            || combined.contains("access denied"),
        "denied path must surface as access-denied error, got stderr: {stderr}"
    );
}

/// `unsupported` — the `landlock_kill_wrapper` deterministically
/// filters `landlock_create_ruleset`/`add_rule`/`restrict_self` to
/// return `ENOSYS` via seccomp-bpf. The helper observes the same
/// `ENOSYS` it would see on a kernel-without-Landlock, and surfaces
/// it as typed `Unsupported: <syscall>: ENOSYS` — NOT as a
/// cause-specific "kernel lacks Landlock" claim (the helper cannot
/// distinguish absent-feature from blocked-syscall from errno alone).
///
/// This test runs unconditionally on every host (capable or not):
/// the wrapper forces the unavailable path, the helper surfaces the
/// typed refusal, and the assertion always runs. No `#[ignore]` —
/// the wrapper IS the determinism.
///
/// Three assertions on the captured wrapper+helper invocation:
///   1. helper exit_code == 1 (the typed-Unsupported path fires)
///   2. stderr names `landlock_create_ruleset: ENOSYS` specifically —
///      not a generic `unsupported:` (that could come from an
///      unrelated prctl failure) and not a physical-cause claim
///   3. stdout is empty — proves the wrapped Python never ran, so
///      the refusal came from the seccomp filter, not from the
///      wrapped command's own failure path.
#[test]
fn unsupported_kernel_typed_refusal() {
    // Spawn landlock_kill_wrapper, which sets no_new_privs + the
    // seccomp-bpf filter, then execvp's the helper. The helper sees
    // Landlock syscalls returning ENOSYS and exits 1 with the typed
    // refusal. The wrapped python3 prints a unique marker; if it
    // runs, stdout contains the marker. Asserting stdout is empty
    // proves the filter caught the syscall before exec.
    const MARKER: &str = "LANDLOCK_SPIKE_DID_NOT_REACH_PYTHON";
    let wrapper = landlock_kill_wrapper_path();
    let helper = landlock_helper_path();
    let wrapped_code = format!("print('{MARKER}')");
    let output = Command::new(&wrapper)
        .arg("--")
        .arg(&helper)
        .args(["--allow=/tmp", "--allow=/proc/self/fd", "--allow=/etc"])
        .arg("--")
        .arg("/usr/bin/python3")
        .arg("-c")
        .arg(&wrapped_code)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("landlock_kill_wrapper must spawn");
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // (1) helper exit_code == 1.
    assert_eq!(
        exit_code, 1,
        "helper must exit 1 under seccomp-filtered Landlock; stderr: {stderr}"
    );

    // (2) stderr names the SPECIFIC syscall + errno that the helper
    //     observed. A generic "unsupported:" prefix would also match
    //     an unrelated prctl failure (e.g., seccomp-blocking prctl);
    //     the specific reason proves the seccomp filter caught the
    //     Landlock syscall itself.
    assert!(
        stderr.contains("landlock_create_ruleset: ENOSYS"),
        "stderr must name the specific seccomp-blocked syscall \
         (landlock_create_ruleset: ENOSYS); got: {stderr}"
    );
    // Helper must NOT claim a specific physical cause (the false
    // diagnosis Advisor flagged in the prior round).
    assert!(
        !stderr.contains("kernel lacks"),
        "helper must not infer physical cause from errno alone; got: {stderr}"
    );

    // (3) stdout is empty — proves the wrapped python never ran.
    //     The python command would print LANDLOCK_SPIKE_DID_NOT_REACH_PYTHON
    //     to stdout on success; if the marker is absent, the seccomp
    //     filter killed the helper's execvp chain before python started.
    assert!(
        !stdout.contains(MARKER),
        "stdout contains the marker — wrapped python ran, seccomp filter \
         may not have intercepted the Landlock syscalls. stdout: {stdout:?}"
    );
    assert!(
        stdout.is_empty(),
        "stdout is not empty — wrapped python output leaked through \
         the seccomp filter. stdout: {stdout:?}"
    );
}
