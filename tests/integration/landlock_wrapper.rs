//! Shipped Landlock wrapper fast tests (task 3.2).
//!
//! These drive the REAL `cistella-landlock-wrap` binary directly on
//! this seat (kernel Landlock, seccomp off — same basis as the
//! spike's local smoke): probe shape, usage refusals, admitted
//! apply, and the denial matrix (write, create, unlink, truncate)
//! with attestation on the diagnostics fd. Podman-seat variants
//! (in-container probe/apply, preserve-fds forwarding) ride the
//! 3.4 live tier on the operator-authorized seat.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Resolves the wrapper binary: `target/<profile>/` sorted by
/// mtime descending (same selection as the peer helpers; the
/// duplication is small and documented there).
fn wrapper_path() -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let profile_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(profile_dir) {
        for entry in entries {
            let entry = entry.expect("dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != "cistella-landlock-wrap" && !name.starts_with("cistella-landlock-wrap-") {
                continue;
            }
            let metadata = entry.metadata().expect("metadata");
            if !metadata.is_file() || (metadata.mode() & 0o111) == 0 {
                continue;
            }
            candidates.push((metadata.modified().expect("mtime"), entry.path()));
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates.into_iter().next().map(|(_, path)| path).unwrap_or_else(|| {
        panic!(
            "cistella-landlock-wrap executable not found in {}; run `cargo build --bin cistella-landlock-wrap` first",
            profile_dir.display()
        )
    })
}

/// Runs the wrapper ancestor mode with diagnostics on fd 1
/// (captured stdout): returns `(exit, attestation_line,
/// rest_stdout, stderr)`. The harness is python3; `/usr` is the
/// read-execute route, `allowed` the read-write route.
fn run_wrapped(allowed: &std::path::Path, python: &str) -> (i32, String, String, String) {
    let output = Command::new(wrapper_path())
        .arg("--diagnostics-fd=1")
        .arg("--allow-ro=/usr")
        .arg(format!("--allow-rw={}", allowed.display()))
        .arg("--")
        .arg("/usr/bin/python3")
        .arg("-c")
        .arg(python)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("wrapper must spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines = stdout.splitn(2, '\n');
    let attestation = lines.next().unwrap_or("").to_string();
    let rest = lines.next().unwrap_or("").to_string();
    (
        output.status.code().unwrap_or(-1),
        attestation,
        rest,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Scratch tree with `allowed/` (rw) and `denied/` (no rule)
/// siblings under one tempdir.
fn scratch_tree() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let allowed = dir.path().join("allowed");
    std::fs::create_dir_all(&allowed).expect("allowed dir");
    std::fs::create_dir_all(dir.path().join("denied")).expect("denied dir");
    (dir, allowed)
}

#[test]
fn probe_reports_usable_abi_and_full_mask() {
    let output = Command::new(wrapper_path())
        .arg("--probe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("probe must spawn");
    assert_eq!(output.status.code(), Some(0), "probe must exit 0");
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("probe prints JSON");
    let abi = payload
        .get("abi")
        .and_then(|abi| abi.as_u64())
        .expect("abi number");
    assert!(abi >= 3, "matrix needs TRUNCATE ABI, got {abi}");
    let mask = payload
        .get("handled_fs_mask")
        .and_then(|mask| mask.as_u64())
        .expect("mask number");
    assert_eq!(mask, (1 << 15) - 1, "handled mask must be bits 0..=14");
}

#[test]
fn usage_errors_exit_3_without_applying() {
    for args in [
        vec![],
        vec!["--diagnostics-fd=1".to_string()],
        vec!["--diagnostics-fd=nope".to_string(), "--".to_string()],
        vec![
            "--diagnostics-fd=1".to_string(),
            "--allow-rw=/tmp".to_string(),
        ],
    ] {
        let output = Command::new(wrapper_path())
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .expect("wrapper must spawn");
        assert_eq!(
            output.status.code(),
            Some(3),
            "usage error must exit 3, args: {args:?}"
        );
    }
}

#[test]
fn admitted_write_succeeds_attested() {
    let (_dir, allowed) = scratch_tree();
    let marker = allowed.join("marker");
    let (exit, attestation, _, _) = run_wrapped(
        &allowed,
        &format!("open({:?}, 'w').write('confined-ok')", marker),
    );
    assert_eq!(exit, 0, "admitted harness must exit 0");
    let payload: serde_json::Value =
        serde_json::from_str(&attestation).expect("first stdout line is attestation JSON");
    assert_eq!(payload.get("applied"), Some(&serde_json::Value::Bool(true)));
    assert_eq!(
        std::fs::read_to_string(&marker)
            .expect("marker readable")
            .as_str(),
        "confined-ok"
    );
}

#[test]
fn sibling_create_denied_attested() {
    let (dir, allowed) = scratch_tree();
    let target = dir.path().join("denied").join("x");
    let (exit, attestation, _, _) = run_wrapped(
        &allowed,
        &format!("open({:?}, 'w').write('escape')", target),
    );
    // Python exits 1 on PermissionError; the wrapper exec'd it, so
    // the harness status propagates while confinement held.
    assert_ne!(exit, 0, "denied create must fail the harness");
    assert!(!target.exists(), "denied file must not exist");
    let payload: serde_json::Value =
        serde_json::from_str(&attestation).expect("attestation precedes harness output");
    assert_eq!(payload.get("applied"), Some(&serde_json::Value::Bool(true)));
}

#[test]
fn sibling_read_and_truncate_denied_file_unmodified() {
    let (dir, allowed) = scratch_tree();
    let sibling = dir.path().join("denied").join("sib.txt");
    std::fs::write(&sibling, "SIB").expect("sibling fixture");
    let (exit, _, _, _) = run_wrapped(&allowed, &format!("open({:?}, 'r').read()", sibling));
    assert_ne!(exit, 0, "sibling read must fail");
    let (exit, _, _, _) = run_wrapped(
        &allowed,
        &format!(
            "import os; os.open({:?}, os.O_RDONLY | os.O_TRUNC)",
            sibling
        ),
    );
    assert_ne!(exit, 0, "sibling O_RDONLY|O_TRUNC must fail");
    assert_eq!(
        std::fs::read_to_string(&sibling)
            .expect("sibling readable")
            .as_str(),
        "SIB",
        "denied truncate must leave the file unmodified"
    );
}

#[test]
fn sibling_unlink_denied() {
    let (dir, allowed) = scratch_tree();
    let sibling = dir.path().join("denied").join("gone.txt");
    std::fs::write(&sibling, "GONE").expect("sibling fixture");
    let (exit, _, _, _) = run_wrapped(&allowed, &format!("import os; os.unlink({:?})", sibling));
    assert_ne!(exit, 0, "sibling unlink must fail");
    assert!(sibling.exists(), "denied unlink must leave the file");
}

#[test]
fn allowed_truncate_succeeds() {
    let (_dir, allowed) = scratch_tree();
    let target = allowed.join("truncme.txt");
    std::fs::write(&target, "OLD").expect("truncate fixture");
    let (exit, _, _, _) = run_wrapped(
        &allowed,
        &format!("f = open({:?}, 'r+'); f.truncate(0)", target),
    );
    assert_eq!(exit, 0, "truncate inside the rw route must succeed");
    assert_eq!(
        std::fs::read_to_string(&target)
            .expect("target readable")
            .as_str(),
        ""
    );
}

#[test]
fn bad_allow_path_fails_before_exec() {
    // Nonexistent allow route: apply fails, the harness never runs
    // (no marker), exit is the apply class.
    let output = Command::new(wrapper_path())
        .arg("--diagnostics-fd=1")
        .arg("--allow-ro=/usr")
        .arg("--allow-rw=/nonexistent-route-xyz")
        .arg("--")
        .arg("/usr/bin/python3")
        .arg("-c")
        .arg("open('/tmp/should-never-exist-xyz', 'w')")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("wrapper must spawn");
    assert_eq!(
        output.status.code(),
        Some(1),
        "bad allow path is apply-class"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let payload: serde_json::Value =
        serde_json::from_str(stdout.lines().next().unwrap_or("")).expect("failure attestation");
    assert_eq!(
        payload.get("applied"),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(!std::path::Path::new("/tmp/should-never-exist-xyz").exists());
}

#[test]
fn injection_path_stays_framed_json() {
    // Quote/newline/control bytes in an allow path must not corrupt
    // the diagnostics frame: serde owns escaping, and the first
    // stdout line still parses as a typed applied:false refusal
    // with no exec (marker absent).
    let (_dir, _allowed) = scratch_tree();
    let evil = "/nonexistent-\"}\n,\"x\":\"y";
    let output = Command::new(wrapper_path())
        .arg("--diagnostics-fd=1")
        .arg("--allow-ro=/usr")
        .arg(format!("--allow-rw={evil}"))
        .arg("--")
        .arg("/usr/bin/python3")
        .arg("-c")
        .arg("open('/tmp/landlock-injection-escaped-xyz', 'w')")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("wrapper must spawn");
    assert_eq!(output.status.code(), Some(1), "bad allow is apply-class");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let payload: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap_or(""))
        .expect("failure line parses despite injection");
    assert_eq!(
        payload.get("applied"),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(!std::path::Path::new("/tmp/landlock-injection-escaped-xyz").exists());
}

#[test]
fn broken_diagnostics_exits_without_execing() {
    // The diagnostics fd number is never open in the child: the
    // attestation write fails, and the wrapper exits without
    // execing (marker absent). Exit is the post-apply class.
    let (_dir, allowed) = scratch_tree();
    let marker = allowed.join("noexec-marker");
    let output = Command::new(wrapper_path())
        .arg("--diagnostics-fd=99")
        .arg("--allow-ro=/usr")
        .arg(format!("--allow-rw={}", allowed.display()))
        .arg("--")
        .arg("/usr/bin/python3")
        .arg("-c")
        .arg(format!("open({:?}, 'w')", marker))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("wrapper must spawn");
    assert_eq!(
        output.status.code(),
        Some(2),
        "broken attestation is post-apply"
    );
    assert!(
        !marker.exists(),
        "harness must never exec past a failed write"
    );
}

#[test]
fn exec_failure_reports_wrapper_error() {
    // Applied, supervised, then a bad harness path: exit 2 with a
    // typed transitioned:false second line (wrapper failure, never
    // a harness outcome). The transitioned vocabulary (not
    // applied:false) classifies post-attestation failures.
    let (_dir, allowed) = scratch_tree();
    let output = Command::new(wrapper_path())
        .arg("--diagnostics-fd=1")
        .arg("--allow-ro=/usr")
        .arg(format!("--allow-rw={}", allowed.display()))
        .arg("--")
        .arg("/nonexistent-harness-xyz")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("wrapper must spawn");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exec failure is wrapper-class"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines = stdout.lines();
    let attested: serde_json::Value =
        serde_json::from_str(lines.next().unwrap_or("")).expect("attestation first");
    assert_eq!(
        attested.get("applied"),
        Some(&serde_json::Value::Bool(true))
    );
    let failed: serde_json::Value =
        serde_json::from_str(lines.next().unwrap_or("")).expect("transition second");
    assert_eq!(
        failed.get("transitioned"),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(
        failed
            .get("error")
            .and_then(|error| error.as_str())
            .is_some()
    );
}

#[test]
fn success_emits_transitioned_true() {
    // Positive exec-transition proof shape: attestation, then the
    // supervisor's transitioned:true, then EOF. The host gate
    // requires all three.
    let (_dir, allowed) = scratch_tree();
    let output = Command::new(wrapper_path())
        .arg("--diagnostics-fd=1")
        .arg("--allow-ro=/usr")
        .arg(format!("--allow-rw={}", allowed.display()))
        .arg("--")
        .arg("/usr/bin/python3")
        .arg("-c")
        .arg("pass")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("wrapper must spawn");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines = stdout.lines();
    let attested: serde_json::Value =
        serde_json::from_str(lines.next().unwrap_or("")).expect("attestation first");
    assert_eq!(
        attested.get("applied"),
        Some(&serde_json::Value::Bool(true))
    );
    let transitioned: serde_json::Value =
        serde_json::from_str(lines.next().unwrap_or("")).expect("transition second");
    assert_eq!(
        transitioned.get("transitioned"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(lines.next(), None, "stream ends after transition");
}

#[test]
fn fault_kill_after_attest_dies_by_signal_without_execing() {
    // Deterministic pin of the crash-between-seal-and-exec window:
    // the wrapper attests, then SIGKILLs itself instead of execing.
    // The process dies by signal (never a clean exit, never an
    // exec): the host side must report session-signal, never a
    // fabricated harness outcome.
    let (_dir, allowed) = scratch_tree();
    let marker = allowed.join("fault-marker");
    let output = Command::new(wrapper_path())
        .arg("--diagnostics-fd=1")
        .arg("--allow-ro=/usr")
        .arg(format!("--allow-rw={}", allowed.display()))
        .arg("--")
        .arg("/usr/bin/python3")
        .arg("-c")
        .arg(format!("open({:?}, 'w')", marker))
        .env("CISTELLA_QA_WRAPPER_FAULT", "kill-after-attest")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("wrapper must spawn");
    assert_eq!(
        output.status.code(),
        None,
        "fault must die by signal, not exit"
    );
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(output.status.signal(), Some(9), "fault must be SIGKILL");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let attested: serde_json::Value =
        serde_json::from_str(stdout.lines().next().unwrap_or("")).expect("attestation first");
    assert_eq!(
        attested.get("applied"),
        Some(&serde_json::Value::Bool(true))
    );
    assert!(
        !marker.exists(),
        "harness must never exec in the fault window"
    );
}
