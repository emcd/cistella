//! Real guest-binary hello pins: conditional capability advertisement.
//!
//! Drives the production `cistella-isolator-podman` binary (no
//! podman/systemd needed — hello precedes all lifecycle): the guest
//! advertises `await-interrupted-v1` ONLY when the host offered it,
//! so an old host admitting exactly the offered set negotiates
//! cleanly into legacy downgrade. Deterministic subprocess pair.

use std::time::Duration;

use cistella::framework::contract::Deadlines;
use cistella::framework::guest::host_external;
use cistella::framework::protocol::{
    AWAIT_INTERRUPTED_V1, Envelope, PRE_NEGOTIATION_MAX_FRAME, PROTOCOL_MAJOR, envelope_bytes,
    parse_envelope, read_frame, write_frame,
};

/// Resolves the built guest binary next to the test executable
/// (`target/<profile>/cistella-isolator-podman`).
fn guest_bin() -> std::path::PathBuf {
    let my_path = std::env::current_exe().expect("current_exe");
    let profile_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors");
    let bin = profile_dir.join("cistella-isolator-podman");
    assert!(
        bin.is_file(),
        "guest binary missing at {}; run `cargo build --bins` first",
        bin.display()
    );
    bin
}

fn tight_deadlines() -> Deadlines {
    Deadlines {
        hello: Duration::from_secs(10),
        plan: Duration::from_secs(2),
        apply: Duration::from_secs(2),
        terminate_grace: Duration::from_secs(2),
        frame_completion: Duration::from_secs(60),
    }
}

/// Exchanges one hello with the real guest binary and returns the
/// advertised capabilities.
fn hello_exchange(offered: &[&str]) -> Vec<String> {
    let bin = guest_bin();
    let mut child = std::process::Command::new(&bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn guest");
    let request = Envelope {
        protocol: PROTOCOL_MAJOR,
        id: "hello-pin".to_string(),
        op: "hello".to_string(),
        payload: serde_json::json!({
            "version": PROTOCOL_MAJOR,
            "capabilities": offered,
        }),
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    write_frame(
        &mut stdin,
        &envelope_bytes(&request),
        PRE_NEGOTIATION_MAX_FRAME,
    )
    .expect("write hello");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let body = read_frame(&mut stdout, PRE_NEGOTIATION_MAX_FRAME, HELLO_WAIT)
        .expect("read hello response");
    // Clean EOF with no live state exits quietly: closing stdin
    // before any op must reap exit 0.
    drop(stdin);
    drop(stdout);
    let status = child.wait().expect("reap guest");
    assert!(
        status.success(),
        "guest exits quietly after hello: {status:?}"
    );
    let response = parse_envelope(&body).expect("parse hello response");
    assert_eq!(response.op, "hello");
    response.payload["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .map(|name| name.as_str().expect("capability string").to_string())
        .collect()
}

const HELLO_WAIT: Duration = Duration::from_secs(10);

#[test]
fn guest_advertises_interrupted_only_when_offered() {
    let advertised = hello_exchange(&["isolator", AWAIT_INTERRUPTED_V1]);
    assert!(
        advertised.contains(&AWAIT_INTERRUPTED_V1.to_string()),
        "offered capability advertised: {advertised:?}"
    );
}

#[test]
fn guest_advertises_role_only_when_unoffered() {
    let advertised = hello_exchange(&["isolator"]);
    assert_eq!(
        advertised,
        vec!["isolator".to_string()],
        "unoffered capability stays silent (old hosts admit exactly the offered set)"
    );
}

#[test]
fn old_host_admission_negotiates_legacy_cleanly() {
    // The old-host shape: offered set WITHOUT the new capability,
    // production admission path. Must negotiate (not refuse) with
    // exactly the role advertised — legacy downgrade then applies
    // end to end.
    let bin = guest_bin();
    let dir = bin.parent().expect("binary has a parent").to_path_buf();
    let name = bin
        .file_name()
        .expect("binary file name")
        .to_string_lossy()
        .into_owned();
    let (mut host, negotiated) = host_external(
        &dir,
        &name,
        &[],
        &["isolator".to_string()],
        tight_deadlines(),
    )
    .expect("old-shape hello must negotiate, not refuse");
    assert_eq!(negotiated, vec!["isolator".to_string()]);
    let cleanup = host.shutdown();
    assert!(cleanup.is_ok(), "shutdown must reap: {cleanup:?}");
}
