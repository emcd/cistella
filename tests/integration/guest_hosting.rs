//! External-guest hosting through the production path (task 1.2).
//!
//! Drives `framework::guest::host_external` against the deterministic
//! peer sitting in the examples directory (the peer binary name is a
//! bare file name, so the examples dir serves as the install sibling
//! directory here): happy-path hello negotiates, and a
//! version-mismatched hello refuses with no guest left behind.

use std::time::Duration;

use cistella::framework::contract::Deadlines;
use cistella::framework::guest::host_external;

use super::protocol_peer::peer_path;

fn tight_deadlines() -> Deadlines {
    Deadlines {
        hello: Duration::from_secs(2),
        plan: Duration::from_secs(2),
        apply: Duration::from_secs(2),
        terminate_grace: Duration::from_secs(2),
    }
}

/// The examples directory holding the peer binary.
fn examples_dir() -> std::path::PathBuf {
    peer_path()
        .parent()
        .expect("peer has a parent directory")
        .to_path_buf()
}

#[test]
fn host_external_hello_negotiates() {
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    // The peer must advertise real capability names: the production
    // path closes negotiation against the offered set.
    let offered = [
        "environment",
        "mounts",
        "policy-claims",
        "guest-hooks",
        "credentials",
    ]
    .map(String::from)
    .to_vec();
    let mut host = host_external(
        &dir,
        &name,
        &["--mode=hello-real-capabilities".to_string()],
        &offered,
        tight_deadlines(),
    )
    .expect("hello must negotiate with the peer");
    let cleanup = host.shutdown();
    assert!(cleanup.is_ok(), "shutdown must reap: {cleanup:?}");
}

#[test]
fn host_external_version_mismatch_refuses_clean() {
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let error = match host_external(
        &dir,
        &name,
        &["--mode=hello-version-mismatch".to_string()],
        &["environment".to_string()],
        tight_deadlines(),
    ) {
        Ok(_) => panic!("version mismatch must refuse"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("version") || message.contains("protocol"),
        "typed version refusal, got: {message}"
    );
}

#[test]
fn host_external_bad_capability_refuses_clean() {
    // Closed negotiation: the peer advertises an unknown capability
    // name, so the production path must refuse with a typed
    // capability error (distinct from the version error) after
    // shutting the guest down. The offered set is real; the
    // advertisement is not within it.
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let offered = [
        "environment",
        "mounts",
        "policy-claims",
        "guest-hooks",
        "credentials",
    ]
    .map(String::from)
    .to_vec();
    let error = match host_external(
        &dir,
        &name,
        &["--mode=hello-bad-capability".to_string()],
        &offered,
        tight_deadlines(),
    ) {
        Ok(_) => panic!("unknown capability must refuse"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("capability") && !message.contains("version"),
        "typed capability refusal distinct from version, got: {message}"
    );
    assert!(
        !message.contains("not-a-real-capability"),
        "refusal must not echo guest bytes, got: {message}"
    );
    // A fresh hello negotiates immediately after the refusal:
    // shared host state is not wedged by the refused guest. (This
    // proves no wedged state, not process reaping — reaping is
    // pinned by the liveness test below.)
    let mut host = host_external(
        &dir,
        &name,
        &["--mode=hello-real-capabilities".to_string()],
        &offered,
        tight_deadlines(),
    )
    .expect("seat must negotiate after a refusal");
    assert!(host.shutdown().is_ok(), "shutdown must reap");
}

/// True when any live process carries `token` in its cmdline.
/// Linux-only (reads /proc); no new host API is required to observe
/// reaping.
#[cfg(target_os = "linux")]
fn process_with_arg(token: &str) -> bool {
    let entries = std::fs::read_dir("/proc").expect("/proc must list");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(cmd) = std::fs::read(entry.path().join("cmdline"))
            && cmd
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        {
            return true;
        }
    }
    false
}

#[cfg(target_os = "linux")]
#[test]
fn host_external_evil_capability_refuses_value_free_and_reaps() {
    // The peer advertises control bytes plus a fake secret
    // assignment, then lingers 60s (it never self-exits): refusal
    // diagnostics must not render those bytes, and the lingering
    // process must be gone afterwards — proving the refusal path's
    // shutdown reaped it rather than the peer exiting on its own.
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let offered = [
        "environment",
        "mounts",
        "policy-claims",
        "guest-hooks",
        "credentials",
    ]
    .map(String::from)
    .to_vec();
    let error = match host_external(
        &dir,
        &name,
        &["--mode=hello-evil-capability".to_string()],
        &offered,
        tight_deadlines(),
    ) {
        Ok(_) => panic!("evil capability must refuse"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "protocol: unsupported guest capability at index 0",
        "fixed-class refusal, no guest bytes"
    );
    let waited = std::time::Instant::now();
    while process_with_arg("hello-evil-capability") {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(10),
            "lingering refused guest must be reaped"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn host_external_peer_death_is_typed_bounded_and_rehostable() {
    // Hosting-layer death semantics (task 1.3, guest-agnostic):
    // SIGKILL the peer mid-exchange, then pin that a subsequent
    // request fails with a bounded typed error (no hang), shutdown
    // after death is clean, and a fresh host negotiates (no wedged
    // state). Op-level recovery by key rides with the Podman guest.
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let offered = [
        "environment",
        "mounts",
        "policy-claims",
        "guest-hooks",
        "credentials",
    ]
    .map(String::from)
    .to_vec();
    let mut host = host_external(
        &dir,
        &name,
        &["--mode=hello-real-capabilities".to_string()],
        &offered,
        tight_deadlines(),
    )
    .expect("hello must negotiate before the kill");
    let pid = host.pid();
    let kill = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .output()
        .expect("kill must spawn");
    assert!(kill.status.success(), "SIGKILL must land");
    let start = std::time::Instant::now();
    let error = match host.exchange_mut().request(
        "ping",
        serde_json::json!({"ok": true}),
        std::time::Duration::from_secs(5),
    ) {
        Ok(_) => panic!("request to a dead peer must fail"),
        Err(error) => error,
    };
    assert!(
        start.elapsed() < std::time::Duration::from_secs(30),
        "failure must be bounded, not a hang"
    );
    assert!(
        error.to_string().contains("protocol"),
        "typed protocol failure, got: {error}"
    );
    assert!(
        host.shutdown().is_ok(),
        "shutdown after death must reap cleanly"
    );
    let mut fresh = host_external(
        &dir,
        &name,
        &["--mode=hello-real-capabilities".to_string()],
        &offered,
        tight_deadlines(),
    )
    .expect("re-host must negotiate after death");
    assert!(fresh.shutdown().is_ok(), "fresh shutdown must reap");
}
