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
        frame_completion: Duration::from_secs(60),
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

/// Directory holding built binaries (`target/<profile>/`): the
/// peer lives in `examples/`, the guest binary beside it.
fn bins_dir() -> std::path::PathBuf {
    peer_path()
        .parent()
        .expect("examples dir")
        .parent()
        .expect("profile dir")
        .to_path_buf()
}

/// Resolves the built isolator guest binary (built by the normal
/// test-target build; a missing binary is an environment bug, not
/// a skip — fail loudly with the build instruction).
fn guest_bin() -> (std::path::PathBuf, String) {
    let dir = bins_dir();
    let name = cistella::isolators::client::ISOLATOR_BIN.to_string();
    assert!(
        dir.join(&name).exists(),
        "guest binary missing: run `cargo build --bin {name}` first"
    );
    (dir, name)
}

#[test]
fn wire_client_hosts_real_guest_and_closes() {
    // Full client lifecycle against the real guest binary with no
    // podman: rendezvous bind, hello negotiation, pid-bound
    // accept, orderly close with rendezvous cleanup.
    use cistella::isolators::client::WireClient;
    let (dir, _name) = guest_bin();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let client = WireClient::host(&dir, rendezvous.path(), tight_deadlines())
        .expect("host must negotiate with the real guest");
    client.close().expect("close must shut down and clean up");
}

#[test]
fn wire_client_maps_backend_error_envelope() {
    // A backend failure inside the guest (podman absent in-seat)
    // crosses as a typed error envelope and reconstructs
    // framework-side with its class intact — no podman needed to
    // prove the error path, only to prove success.
    use cistella::framework::contract::ReconciliationKey;
    use cistella::framework::isolator::{CreateSpec, Isolator};
    use cistella::isolators::client::WireClient;
    use cistella::session::Session;
    let (dir, _name) = guest_bin();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let client = WireClient::host(&dir, rendezvous.path(), tight_deadlines())
        .expect("host must negotiate with the real guest");
    let spec = CreateSpec {
        session: Session {
            id: "wireclient01".to_string(),
            directory: "/tmp/wireclient01".to_string(),
            profile: "probe".to_string(),
            profile_digest: "digest".to_string(),
            identity: "tester".to_string(),
            command: vec!["true".to_string()],
            image: "localhost/cistella/opencode:example".to_string(),
            container_home: "/home/cistella".to_string(),
        },
        volumes: vec![],
        env: vec![],
        labels: vec![],
    };
    let error = client
        .create(&spec, &ReconciliationKey::generate())
        .expect_err("absent podman must surface as a typed backend error");
    assert!(
        error.to_string().contains("runtime"),
        "backend error class preserved across the wire, got: {error}"
    );
    client.close().expect("close must shut down and clean up");
}

#[test]
fn wire_client_terminate_serves_during_pending_await() {
    // Dispatcher concurrency through WireClient: a slow await
    // pends while terminate sends on the same client — the await
    // must not wedge the control plane. The scripted peer sleeps
    // 3s after the await op and requires the terminate op inside
    // that window (it exits 2 otherwise), then answers terminate
    // first and the await terminal second.
    use cistella::framework::contract::ReconciliationKey;
    use cistella::framework::isolator::{ExecutionOutcome, Isolator};
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let mut deadlines = tight_deadlines();
    deadlines.apply = Duration::from_secs(15);
    let client = std::sync::Arc::new(
        WireClient::host_as(
            &dir,
            &name,
            &[
                "--mode=isolator-concurrent".to_string(),
                format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
            ],
            rendezvous.path(),
            deadlines,
        )
        .expect("host must negotiate with the scripted peer"),
    );
    let execution = cistella::framework::contract::ExecutionHandle::mint();
    let waiter = {
        let client = std::sync::Arc::clone(&client);
        let execution = execution.clone();
        std::thread::spawn(move || {
            let cancel = cistella::framework::contract::CancelFlag::new();
            client.await_result(&execution, &cancel)
        })
    };
    // Let the await op reach the peer and pend, then terminate
    // through the same client: the dispatcher must send it while
    // the await is still outstanding.
    std::thread::sleep(Duration::from_millis(500));
    let key = ReconciliationKey::generate();
    let unit = cistella::framework::contract::UnitHandle::mint();
    let stopped = client
        .terminate(&unit, Duration::from_secs(5), &key)
        .expect("terminate must serve during a pending await");
    assert_eq!(stopped.unit_identity, "concurrent01");
    let outcome = waiter
        .join()
        .expect("waiter joins")
        .expect("await completes");
    assert_eq!(outcome, ExecutionOutcome::Exited(0));
    drop(client);
}

#[test]
fn wire_client_accept_timeout_is_typed() {
    // A guest that answers hello but never connects to the
    // rendezvous path must fail hosting within budget — never
    // wedge conduct. The concurrent peer without --fd-watch
    // negotiates hello (isolator cap) and then blocks on ops,
    // never connecting; accept expires, the rendezvous path
    // cleans up, and the guest is reaped on drop.
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let mut deadlines = tight_deadlines();
    deadlines.hello = Duration::from_secs(2);
    let before = std::time::Instant::now();
    let error = match WireClient::host_as(
        &dir,
        &name,
        &["--mode=isolator-concurrent".to_string()],
        rendezvous.path(),
        deadlines,
    ) {
        Ok(_) => panic!("hello-only peer must fail accept"),
        Err(error) => error,
    };
    assert!(
        before.elapsed() < Duration::from_secs(30),
        "accept failure is bounded"
    );
    let message = error.to_string();
    assert!(
        message.contains("timed out") || message.contains("died"),
        "typed accept failure, got: {message}"
    );
    assert!(
        std::fs::read_dir(rendezvous.path())
            .expect("rendezvous dir lists")
            .next()
            .is_none(),
        "rendezvous path cleans up on refusal"
    );
}

#[test]
fn wire_client_guest_death_fails_pending_without_hang() {
    // The scripted peer dies abruptly after its first op: the
    // dispatcher must fail every pending caller (one uncapped
    // await plus one bounded op) with a typed error instead of
    // treating death as silence. Neither caller may hang.
    use cistella::framework::contract::{ExecutionHandle, UnitHandle};
    use cistella::framework::isolator::Isolator;
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let rendezvous = tempfile::tempdir().expect("tempdir");
    let mut deadlines = tight_deadlines();
    deadlines.apply = Duration::from_secs(15);
    let client = std::sync::Arc::new(
        WireClient::host_as(
            &dir,
            &name,
            &[
                "--mode=isolator-die-mid-await".to_string(),
                format!("--fd-watch={}", rendezvous.path().to_string_lossy()),
            ],
            rendezvous.path(),
            deadlines,
        )
        .expect("host must negotiate with the scripted peer"),
    );
    let execution = ExecutionHandle::mint();
    let waiter = {
        let client = std::sync::Arc::clone(&client);
        let execution = execution.clone();
        std::thread::spawn(move || {
            let cancel = cistella::framework::contract::CancelFlag::new();
            client.await_result(&execution, &cancel)
        })
    };
    // Give the await op time to arrive and the peer time to die,
    // then issue a bounded op: both must fail typed, neither hang.
    std::thread::sleep(Duration::from_secs(2));
    let unit = UnitHandle::mint();
    let bounded = client.inspect(&unit);
    assert!(
        bounded.is_err(),
        "bounded op after guest death must fail typed"
    );
    let awaited = waiter
        .join()
        .expect("waiter joins")
        .expect_err("uncapped await after guest death must fail typed");
    assert!(
        awaited.to_string().contains("guest terminated")
            || awaited.to_string().contains("dispatcher"),
        "guest-death error dominates, got: {awaited}"
    );
    drop(client);
}

/// Hosts the scripted peer in the given mode (no fd channel
/// needed: these modes never launch). The rendezvous directory is
/// leaked deliberately: the client holds its path for the session
/// and a dropped TempDir would remove it mid-test.
fn host_scripted(
    mode: &str,
    deadlines: cistella::framework::contract::Deadlines,
) -> cistella::isolators::client::WireClient {
    host_scripted_extra(mode, &[], deadlines)
}

/// Hosts the scripted peer with additional argv (fd-watch is
/// always included for rendezvous cooperation).
fn host_scripted_extra(
    mode: &str,
    extra: &[String],
    deadlines: cistella::framework::contract::Deadlines,
) -> cistella::isolators::client::WireClient {
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let scratch = tempfile::tempdir().expect("tempdir");
    let leaked = Box::leak(Box::new(scratch));
    let mut args = vec![
        format!("--mode={mode}"),
        format!("--fd-watch={}", leaked.path().to_string_lossy()),
    ];
    args.extend(extra.iter().cloned());
    WireClient::host_as(&dir, &name, &args, leaked.path(), deadlines)
        .expect("host must negotiate with the scripted peer")
}

#[test]
fn wire_client_split_frame_resolves() {
    // The peer writes the length header, sleeps past two
    // dispatch slices, then writes the body: the persistent
    // assembler must resolve it (fresh-assembler-per-slice would
    // corrupt correlation here).
    use cistella::framework::contract::LifecycleState;
    use cistella::framework::isolator::Isolator;
    let client = host_scripted("isolator-split-frame", tight_deadlines());
    let unit = cistella::framework::contract::UnitHandle::mint();
    assert_eq!(
        client.state(&unit).expect("split frame must resolve"),
        LifecycleState::Initiated
    );
}

#[test]
fn wire_client_wrong_op_refuses() {
    // The peer answers a live request id with a different op:
    // the dispatcher must refuse the mismatch instead of
    // delivering it to the caller.
    use cistella::framework::isolator::Isolator;
    let client = host_scripted("isolator-wrong-op", tight_deadlines());
    let unit = cistella::framework::contract::UnitHandle::mint();
    let error = client.state(&unit).expect_err("op mismatch must refuse");
    assert!(
        error.to_string().contains("mismatches request"),
        "got: {error}"
    );
}

#[test]
fn wire_client_unsolicited_id_fails_exchange() {
    // The peer emits a response for an id nobody asked about
    // before answering: the dispatcher must fail the exchange
    // (failing the pending caller) rather than drop it
    // silently.
    use cistella::framework::isolator::Isolator;
    let client = host_scripted("isolator-unsolicited", tight_deadlines());
    let unit = cistella::framework::contract::UnitHandle::mint();
    let error = client.state(&unit).expect_err("unsolicited must fail");
    assert!(error.to_string().contains("unsolicited"), "got: {error}");
}

#[test]
fn wire_client_big_frame_routes() {
    // A 2 MiB response (between the 1 MiB default and the
    // negotiated 8 MiB ceiling) routes as a normal response:
    // the dispatcher reads with the negotiated bound.
    use cistella::framework::contract::LifecycleState;
    use cistella::framework::isolator::Isolator;
    let client = host_scripted("isolator-big-frame", tight_deadlines());
    let unit = cistella::framework::contract::UnitHandle::mint();
    assert_eq!(
        client.state(&unit).expect("big frame must route"),
        LifecycleState::Initiated
    );
}

#[test]
fn wire_client_stalled_partial_fails_typed() {
    // The peer answers hello, then sends one header byte and
    // stalls forever: the frame-completion bound (short here,
    // production 60s) must fail the pending call typed instead
    // of leaving the await pending across slices forever.
    use cistella::framework::isolator::Isolator;
    let mut deadlines = tight_deadlines();
    // Frame bound well under the op budget: the stall failure
    // must arrive from framing, not op expiry.
    deadlines.frame_completion = Duration::from_secs(1);
    deadlines.apply = Duration::from_secs(15);
    let client = host_scripted("isolator-await-stall", deadlines);
    let unit = cistella::framework::contract::UnitHandle::mint();
    let start = std::time::Instant::now();
    let error = client.state(&unit).expect_err("stall must fail typed");
    assert!(
        error.to_string().contains("stalled after first byte"),
        "frame-completion failure, got: {error}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "frame bound applies, not the harness lifetime"
    );
}

#[test]
fn wire_client_close_after_guest_death_cleans_up() {
    // The peer exits shortly after hello: close() on the dead
    // dispatcher must still join the worker and unlink the
    // rendezvous path (no strand on any close outcome).
    use cistella::isolators::client::WireClient;
    let dir = examples_dir();
    let name = peer_path()
        .file_name()
        .expect("peer file name")
        .to_string_lossy()
        .into_owned();
    let scratch = tempfile::tempdir().expect("tempdir");
    let leaked = Box::leak(Box::new(scratch));
    let leaked_path = leaked.path().to_path_buf();
    let client = WireClient::host_as(
        &dir,
        &name,
        &[
            "--mode=isolator-exit-after-hello".to_string(),
            format!("--fd-watch={}", leaked_path.to_string_lossy()),
        ],
        &leaked_path,
        tight_deadlines(),
    )
    .expect("host must negotiate before the peer exits");
    // The peer is dead by now; the dispatcher has broken out.
    std::thread::sleep(Duration::from_secs(2));
    let error = client
        .close()
        .expect_err("close after death reports, not Ok");
    let _ = error;
    assert!(
        std::fs::read_dir(&leaked_path)
            .expect("rendezvous dir lists")
            .next()
            .is_none(),
        "rendezvous socket unlinked on every close outcome"
    );
}
