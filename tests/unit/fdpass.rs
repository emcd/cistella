//! Ancillary-FD channel conformance (task 2.2, decision C).
//!
//! Bundle round-trips, count/shape refusals, and rendezvous
//! authentication run over real Unix sockets with no podman: the
//! channel is pure local IPC. Fault injection (wrong counts,
//! garbage headers, fd-less messages) drives raw nix calls to
//! prove the receiver refuses instead of admitting partial
//! bundles.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use cistella::framework::fdpass::{
    BundleHeader, accept_authenticated, bind_rendezvous, connect_rendezvous, recv_bundle,
    send_bundle,
};

fn null_fd() -> OwnedFd {
    std::fs::File::open("/dev/null")
        .expect("/dev/null opens")
        .into()
}

fn header(unit: &str, exec: &str) -> BundleHeader {
    BundleHeader {
        unit_handle: unit.to_string(),
        execution_handle: exec.to_string(),
    }
}

fn pair() -> (OwnedFd, OwnedFd) {
    nix::sys::socket::socketpair(
        nix::sys::socket::AddressFamily::Unix,
        nix::sys::socket::SockType::SeqPacket,
        None,
        nix::sys::socket::SockFlag::empty(),
    )
    .expect("socketpair")
}

#[test]
fn bundle_round_trip() {
    let (a, b) = pair();
    let fds = [null_fd(), null_fd(), null_fd()];
    let borrowed = [fds[0].as_fd(), fds[1].as_fd(), fds[2].as_fd()];
    send_bundle(
        &a,
        &header("unit01", "exec01"),
        &borrowed,
        std::time::Duration::from_secs(2),
    )
    .expect("send");
    let (got_header, got_fds) = recv_bundle(&b, Duration::from_secs(2)).expect("receive");
    assert_eq!(got_header, header("unit01", "exec01"));
    assert_eq!(got_fds.len(), 3);
    for fd in &got_fds {
        assert!(
            nix::sys::stat::fstat(fd.as_raw_fd()).is_ok(),
            "received fds are live references"
        );
    }
}

#[test]
fn bundle_wrong_count_refuses() {
    use nix::sys::socket::{ControlMessage, MsgFlags, UnixAddr, sendmsg};
    use std::io::IoSlice;
    let (a, b) = pair();
    let one = null_fd();
    let body = serde_json::to_vec(&header("unit02", "exec02")).expect("header");
    let iov = [IoSlice::new(&body)];
    sendmsg::<UnixAddr>(
        a.as_raw_fd(),
        &iov,
        &[ControlMessage::ScmRights(&[one.as_raw_fd()])],
        MsgFlags::empty(),
        None,
    )
    .expect("raw send");
    let error = recv_bundle(&b, Duration::from_secs(2)).expect_err("1 fd must refuse");
    assert!(
        error.to_string().contains("exactly 3 descriptors"),
        "got: {error}"
    );
}

#[test]
fn bundle_garbage_header_refuses() {
    use nix::sys::socket::{ControlMessage, MsgFlags, UnixAddr, sendmsg};
    use std::io::IoSlice;
    let (a, b) = pair();
    let one = null_fd();
    let iov = [IoSlice::new(b"{not json")];
    sendmsg::<UnixAddr>(
        a.as_raw_fd(),
        &iov,
        &[ControlMessage::ScmRights(&[
            one.as_raw_fd(),
            one.as_raw_fd(),
            one.as_raw_fd(),
        ])],
        MsgFlags::empty(),
        None,
    )
    .expect("raw send");
    let error = recv_bundle(&b, Duration::from_secs(2)).expect_err("garbage must refuse");
    assert!(
        error.to_string().contains("shape violation"),
        "got: {error}"
    );
}

#[test]
fn bundle_without_fds_refuses() {
    use nix::sys::socket::{MsgFlags, UnixAddr, sendmsg};
    use std::io::IoSlice;
    let (a, b) = pair();
    let body = serde_json::to_vec(&header("unit03", "exec03")).expect("header");
    let iov = [IoSlice::new(&body)];
    sendmsg::<UnixAddr>(a.as_raw_fd(), &iov, &[], MsgFlags::empty(), None).expect("raw send");
    let error = recv_bundle(&b, Duration::from_secs(2)).expect_err("fd-less must refuse");
    assert!(
        error.to_string().contains("exactly 3 descriptors"),
        "got: {error}"
    );
}

#[test]
fn rendezvous_authenticates_same_user() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, path) = bind_rendezvous(dir.path()).expect("bind");
    assert!(
        path.starts_with(dir.path()),
        "socket lives under the 0700 dir"
    );
    let connector = std::thread::spawn({
        let path = path.clone();
        move || connect_rendezvous(&path).expect("connect")
    });
    let accepted =
        accept_authenticated(&listener, std::process::id() as i32).expect("accept same uid");
    let peer = connector.join().expect("connector joins");
    let fds = [null_fd(), null_fd(), null_fd()];
    let borrowed: [BorrowedFd<'_>; 3] = [fds[0].as_fd(), fds[1].as_fd(), fds[2].as_fd()];
    send_bundle(
        &peer,
        &header("unit04", "exec04"),
        &borrowed,
        std::time::Duration::from_secs(2),
    )
    .expect("send");
    let (got, _) = recv_bundle(&accepted, Duration::from_secs(2)).expect("receive");
    assert_eq!(got, header("unit04", "exec04"));
    std::fs::remove_file(&path).expect("rendezvous cleanup");
}

/// Received FDs are usable references, not numbers: data written
/// through a received pipe fd arrives (proves the reference
/// crossed, not just an integer).
#[test]
fn received_fds_are_live_references() {
    let (a, b) = pair();
    let (r, w) = pair();
    let keep = null_fd();
    let borrowed = [r.as_fd(), w.as_fd(), keep.as_fd()];
    send_bundle(
        &a,
        &header("unit05", "exec05"),
        &borrowed,
        std::time::Duration::from_secs(2),
    )
    .expect("send");
    let (_, got) = recv_bundle(&b, Duration::from_secs(2)).expect("receive");
    drop(r);
    drop(w);
    // SAFETY: test-owned fds, libc contract checked.
    unsafe {
        let marker = b"fdpass-live";
        assert_eq!(
            libc::write(
                got[1].as_raw_fd(),
                marker.as_ptr() as *const _,
                marker.len()
            ),
            marker.len() as isize,
            "write through the received fd"
        );
        let mut buf = [0u8; 16];
        assert_eq!(
            libc::read(got[0].as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()),
            marker.len() as isize,
            "read through the received fd"
        );
        assert_eq!(&buf[..marker.len()], marker);
    }
}

#[test]
fn back_to_back_bundles_stay_aligned() {
    // Two bundles queued without a read between them: SEQPACKET
    // record boundaries keep them distinct (no coalescing, no
    // desynchronization).
    let (a, b) = pair();
    let fds = [null_fd(), null_fd(), null_fd()];
    let borrowed = [fds[0].as_fd(), fds[1].as_fd(), fds[2].as_fd()];
    for n in ["backtoback01", "backtoback02"] {
        send_bundle(
            &a,
            &header(n, "exec"),
            &borrowed,
            std::time::Duration::from_secs(2),
        )
        .expect("send");
    }
    for n in ["backtoback01", "backtoback02"] {
        let (got, _) = recv_bundle(&b, Duration::from_secs(2)).expect("receive");
        assert_eq!(got.unit_handle, n, "record order preserved");
    }
}

#[test]
fn over_limit_cmsg_refuses() {
    // Four descriptors against a three-slot control buffer: the
    // truncated fourth must refuse, never read as a valid triple.
    use nix::sys::socket::{ControlMessage, MsgFlags, UnixAddr, sendmsg};
    use std::io::IoSlice;
    let (a, b) = pair();
    let extra = [null_fd(), null_fd(), null_fd(), null_fd()];
    let body = serde_json::to_vec(&header("overlimit01", "exec")).expect("header");
    let iov = [IoSlice::new(&body)];
    let raws: Vec<_> = extra.iter().map(|fd| fd.as_raw_fd()).collect();
    sendmsg::<UnixAddr>(
        a.as_raw_fd(),
        &iov,
        &[ControlMessage::ScmRights(&raws)],
        MsgFlags::empty(),
        None,
    )
    .expect("raw send");
    let error = recv_bundle(&b, Duration::from_secs(2)).expect_err("4 fds must refuse");
    assert!(
        error.to_string().contains("truncated") || error.to_string().contains("exactly 3"),
        "got: {error}"
    );
}

/// Open descriptor count for this process (fd-leak detector).
fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd lists")
        .count()
}

/// Settles the process fd count: polls until two consecutive
/// reads agree, so transient parallel-test descriptors drain out
/// while leaked (never-closed) descriptors persist into the
/// reading. Returns the settled count.
fn settled_fd_count() -> usize {
    let mut previous = open_fd_count();
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let current = open_fd_count();
        if current == previous {
            return current;
        }
        previous = current;
    }
    previous
}

#[test]
fn repeated_refusals_leak_nothing() {
    // Garbage, wrong-count, fd-less, and over-limit bundles
    // refused in a loop must not grow the descriptor table: every
    // received right closes on its refusal path. Counts settle on
    // both sides so parallel-test transients drain; only a
    // persistent leak fails the equality.
    use nix::sys::socket::{ControlMessage, MsgFlags, UnixAddr, sendmsg};
    use std::io::IoSlice;
    let (a, b) = pair();
    let before = settled_fd_count();
    for round in 0..12 {
        let extra = [null_fd(), null_fd(), null_fd(), null_fd()];
        let raws: Vec<_> = extra.iter().map(|fd| fd.as_raw_fd()).collect();
        let body = if round % 3 == 0 {
            b"{not json".to_vec()
        } else {
            serde_json::to_vec(&header("leakprobe", "exec")).expect("header")
        };
        let iov = [IoSlice::new(&body)];
        let cmsgs = if round % 3 == 2 {
            vec![ControlMessage::ScmRights(&raws[..4])]
        } else {
            vec![ControlMessage::ScmRights(&raws[..1])]
        };
        sendmsg::<UnixAddr>(a.as_raw_fd(), &iov, &cmsgs, MsgFlags::empty(), None)
            .expect("raw send");
        let _ = recv_bundle(&b, Duration::from_secs(2)).expect_err("must refuse");
    }
    assert_eq!(
        settled_fd_count(),
        before,
        "refused bundles close every received right"
    );
}

#[test]
fn same_uid_impostor_refuses() {
    // A different process (different pid, same uid) racing the
    // rendezvous path is refused and closed: uid equality alone is
    // not guest identity. The impostor is a python subprocess that
    // connects and lingers; the test's own pid is the expected
    // guest the impostor is not.
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, path) = bind_rendezvous(dir.path()).expect("bind");
    let path_text = path.to_str().expect("socket path UTF-8").to_string();
    let mut impostor = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import socket, time; s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET); s.connect({path_text:?}); time.sleep(30)"
        ))
        .spawn()
        .expect("spawn impostor");
    // Wait for the impostor to connect (poll the accepted queue by
    // attempting a nonblocking accept via a short-poll loop is
    // overkill: the refusal below blocks until someone connects,
    // and only the impostor knows the path).
    let error = accept_authenticated(&listener, std::process::id() as i32)
        .expect_err("impostor pid must refuse");
    assert!(
        error.to_string().contains("identity mismatch"),
        "got: {error}"
    );
    let _ = impostor.kill();
    let _ = impostor.wait();
    std::fs::remove_file(&path).expect("rendezvous cleanup");
}

#[test]
fn saturated_peer_returns_within_budget() {
    // An unreading peer must not wedge the sender past the budget:
    // with a tiny send buffer and no receiver, some send hits the
    // deadline and returns a timeout error instead of hanging.
    use nix::sys::socket::sockopt::SndBuf;
    let (a, _b) = pair();
    nix::sys::socket::setsockopt(&a, SndBuf, &4096).expect("sndbuf");
    let fds = [null_fd(), null_fd(), null_fd()];
    let borrowed = [fds[0].as_fd(), fds[1].as_fd(), fds[2].as_fd()];
    let start = std::time::Instant::now();
    let mut timed_out = false;
    for n in 0..500 {
        let header = BundleHeader {
            unit_handle: format!("saturate{n:03}"),
            execution_handle: "exec".to_string(),
        };
        match send_bundle(&a, &header, &borrowed, Duration::from_secs(2)) {
            Ok(()) => continue,
            Err(error) => {
                assert!(
                    error.to_string().contains("timed out"),
                    "saturated send refuses by deadline, got: {error}"
                );
                timed_out = true;
                break;
            }
        }
    }
    assert!(timed_out, "an unreading peer must trip the deadline");
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "deadline bounds the saturated send"
    );
}

/// Dead-peer send probe: with `SIG_DFL` (the conduct disposition),
/// sending a bundle to a closed peer must surface a typed error,
/// never kill the process and never hang.
/// Control experiment (2026-09-25): `SOCK_SEQPACKET` to a closed
/// peer returns `EPIPE` without raising `SIGPIPE` even under
/// `SIG_DFL`, while `SOCK_STREAM` dies 128+13 — so this test pins
/// typed-error-on-dead-peer, while `MSG_NOSIGNAL` on the send path
/// stays as cheap hygiene against kernel behavioral differences
/// (it cannot be falsified through this socket type by
/// construction).
#[test]
fn dead_peer_send_is_typed() {
    if std::env::var("CISTELLA_FDPASS_SIGPIPE_PROBE").is_ok() {
        // SAFETY: child-mode entry, single purpose: restore the
        // conduct disposition, then probe the closed peer.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        let (a, b) = pair();
        drop(b);
        let fds = [null_fd(), null_fd(), null_fd()];
        let borrowed = [fds[0].as_fd(), fds[1].as_fd(), fds[2].as_fd()];
        let result = send_bundle(
            &a,
            &header("sigpipe01", "exec"),
            &borrowed,
            std::time::Duration::from_secs(5),
        );
        // Survival IS the assertion reaching here: SIGPIPE death
        // would exit by signal instead. The error must be the
        // immediate send-path failure, never the timeout branch
        // (a hung send passing would hide a wedged channel).
        // Marker first: a zero-test child exit must not pass.
        println!("FDPASS-PROBE-EXECUTED");
        match result {
            Ok(()) => std::process::exit(2),
            Err(error) => {
                assert!(
                    error.to_string().contains("send fd bundle"),
                    "immediate send-path failure, got: {error}"
                );
                std::process::exit(0);
            }
        }
    }
    let exe = std::env::current_exe().expect("current test binary");
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("fdpass::dead_peer_send_is_typed")
        .arg("--nocapture")
        .env("CISTELLA_FDPASS_SIGPIPE_PROBE", "1")
        .output()
        .expect("spawn probe child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("FDPASS-PROBE-EXECUTED"),
        "child must execute the probe (zero-test exit cannot pass); status: {:?}, stdout: {stdout:?}",
        output.status
    );
    assert!(
        output.status.success(),
        "dead-peer send must survive SIG_DFL, got: {:?}",
        output.status
    );
}
