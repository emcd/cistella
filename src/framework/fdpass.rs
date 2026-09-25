//! Ancillary-FD channel for session stdio (task 2.2, decision C).
//!
//! Typed stdio bundles cross on a SEPARATE Unix stream socket via
//! `SCM_RIGHTS`, bound to one `execute_launch` request: a JSON
//! header (`unit_handle`, `execution_handle`) plus exactly three
//! file descriptors (stdin/stdout/stderr roles by position).
//! Framed protocol stdio stays control-only; harness bytes never
//! share it.
//!
//! Rendezvous avoids guest-spawn surgery: the framework binds a
//! socket at a random unguessable path under a `0700` directory
//! and passes the path via guest argv; the guest connects once at
//! startup. Peer authentication is `SO_PEERCRED` uid-equality at
//! accept (same-user binding). The framework retains the original
//! FDs until the launch terminal response acknowledges receipt.
//! Linux-only (0.2 Podman is Linux-only); a future non-Unix
//! isolator declares its own stdio transport capability.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::cmsg_space;
use nix::sys::socket::sockopt::PeerCredentials;
use nix::sys::socket::{
    AddressFamily, Backlog, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType,
    UnixAddr, bind, connect, getsockopt, listen, recvmsg, sendmsg, socket,
};
use serde::{Deserialize, Serialize};

use crate::error::{CistellaError, Result};

/// Maximum bundle header bytes (unit plus execution handle JSON).
const MAX_HEADER: usize = 4096;

/// Kernel per-message SCM_RIGHTS cap on Linux (`SCM_MAX_FD`).
/// Sizing the control buffer for this many rights makes
/// truncation unreachable: the kernel refuses larger sends, so
/// every delivered right is enumerable and every refusal path
/// below closes what it received. The `MSG_CTRUNC` refusal stays
/// as defense-in-depth, but it cannot trigger on this kernel.
const MAX_RIGHTS: usize = 253;

/// How long a guest waits for the launch bundle after its wire op.
/// The framework sends the bundle immediately after the op; expiry
/// means the framework died between the two sends (typed failure,
/// never an unbounded wait).
pub const LAUNCH_BUNDLE_WAIT: Duration = Duration::from_secs(30);

/// Bundle header binding one fd triple to one launch request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleHeader {
    /// Framework-issued unit handle string.
    pub unit_handle: String,
    /// Framework-issued execution handle string.
    pub execution_handle: String,
}

/// Binds a rendezvous socket: `0700` directory, random unguessable
/// name, listening. Returns the listener plus the guest-connectable
/// path (removed by the caller when the session ends).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on directory, random-name,
/// bind, or listen failure.
pub fn bind_rendezvous(dir: &Path) -> Result<(OwnedFd, PathBuf)> {
    std::fs::create_dir_all(dir)
        .map_err(|e| CistellaError::Runtime(format!("create fd rendezvous dir: {e}")))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| CistellaError::Runtime(format!("chmod fd rendezvous dir: {e}")))?;
    let mut random = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| {
            use std::io::Read;
            file.read_exact(&mut random)
        })
        .map_err(|e| CistellaError::Runtime(format!("read random socket name: {e}")))?;
    let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("guest-{name}.sock"));
    let listener = socket(
        AddressFamily::Unix,
        SockType::SeqPacket,
        SockFlag::empty(),
        None,
    )
    .map_err(|e| CistellaError::Runtime(format!("create fd socket: {e}")))?;
    let addr = UnixAddr::new(&path)
        .map_err(|e| CistellaError::Runtime(format!("bad socket path: {e}")))?;
    bind(listener.as_raw_fd(), &addr)
        .map_err(|e| CistellaError::Runtime(format!("bind fd socket: {e}")))?;
    listen(
        &listener,
        Backlog::new(1).map_err(|e| CistellaError::Runtime(format!("listen backlog: {e}")))?,
    )
    .map_err(|e| CistellaError::Runtime(format!("listen fd socket: {e}")))?;
    Ok((listener, path))
}

/// Accepts the spawned guest connection with process-identity
/// authentication.
///
/// The accepted socket's peer credentials must report both the
/// current uid AND the exact `expected_pid` (the framework-spawned
/// guest child): another same-uid process racing the rendezvous
/// path is refused and closed, so only the intended guest can
/// receive the session's stdio descriptors. The random socket name
/// plus `0700` directory remain as outer layers, not the identity.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on uid/pid mismatch and
/// `CistellaError::Runtime` on accept failure.
pub fn accept_authenticated(listener: &OwnedFd, expected_pid: i32) -> Result<OwnedFd> {
    let raw = nix::sys::socket::accept(listener.as_raw_fd())
        .map_err(|e| CistellaError::Runtime(format!("accept fd socket: {e}")))?;
    // SAFETY: freshly accepted, owned here, wrapped exactly once.
    // (Held across the checks below so every refusal path closes.)
    let accepted = unsafe { OwnedFd::from_raw_fd(raw) };
    let credentials = getsockopt(&accepted, PeerCredentials)
        .map_err(|e| CistellaError::Runtime(format!("peer credentials: {e}")))?;
    // SAFETY: getuid is inherently safe.
    let me = unsafe { nix::libc::getuid() };
    if credentials.uid() != me || credentials.pid() != expected_pid {
        return Err(CistellaError::Contract(
            "fd channel peer identity mismatch".to_string(),
        ));
    }
    Ok(accepted)
}

/// Connects to a rendezvous path (guest side).
///
/// # Errors
///
/// Returns `CistellaError::Runtime` on socket or connect failure.
pub fn connect_rendezvous(path: &Path) -> Result<OwnedFd> {
    let sock = socket(
        AddressFamily::Unix,
        SockType::SeqPacket,
        SockFlag::empty(),
        None,
    )
    .map_err(|e| CistellaError::Runtime(format!("create fd socket: {e}")))?;
    let addr =
        UnixAddr::new(path).map_err(|e| CistellaError::Runtime(format!("bad socket path: {e}")))?;
    connect(sock.as_raw_fd(), &addr)
        .map_err(|e| CistellaError::Runtime(format!("connect fd socket: {e}")))?;
    Ok(sock)
}

/// Sends one stdio bundle: header bytes plus exactly three FDs in a
/// single `SCM_RIGHTS` message.
///
/// Bounded end to end: the send polls for writability and retries
/// `EAGAIN` under one absolute deadline (`MSG_DONTWAIT` — a bare
/// blocking send could sit past the budget on an unreading peer,
/// since `POLLOUT` never guarantees room for the whole record).
/// `SOCK_SEQPACKET` carries the message atomically; a short send
/// refuses rather than emitting a partial bundle.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on timeout and
/// `CistellaError::Runtime` on send failure.
pub fn send_bundle(
    sock: &OwnedFd,
    header: &BundleHeader,
    fds: &[BorrowedFd<'_>; 3],
    timeout: Duration,
) -> Result<()> {
    use nix::poll::{PollFd, PollFlags, poll};
    let deadline = std::time::Instant::now() + timeout;
    let body =
        serde_json::to_vec(header).map_err(|e| CistellaError::Runtime(format!("header: {e}")))?;
    let raws: Vec<std::os::fd::RawFd> = fds.iter().map(|fd| fd.as_raw_fd()).collect();
    let iov = [IoSlice::new(&body)];
    let sent = loop {
        let mut pollfds = [PollFd::new(sock.as_fd(), PollFlags::POLLOUT)];
        let wait = nix::poll::PollTimeout::try_from(
            deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::ZERO),
        )
        .map_err(|_| CistellaError::Runtime("fd send timeout out of range".to_string()))?;
        if poll(&mut pollfds, wait)
            .map_err(|e| CistellaError::Runtime(format!("poll fd socket: {e}")))?
            == 0
        {
            return Err(CistellaError::Contract(
                "fd bundle send timed out".to_string(),
            ));
        }
        match sendmsg::<UnixAddr>(
            sock.as_raw_fd(),
            &iov,
            &[ControlMessage::ScmRights(&raws)],
            MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL,
            None,
        ) {
            Ok(sent) => break sent,
            Err(nix::errno::Errno::EAGAIN) => continue,
            Err(e) => {
                return Err(CistellaError::Runtime(format!("send fd bundle: {e}")));
            }
        }
    };
    if sent != body.len() {
        return Err(CistellaError::Runtime(format!(
            "short fd bundle send: {sent}/{}",
            body.len()
        )));
    }
    Ok(())
}

/// Receives one stdio bundle with a bounded wait: header plus
/// exactly three FDs, or refusal.
///
/// `SOCK_SEQPACKET` delivers each bundle atomically: no partial
/// headers, no coalesced bundles, no desynchronization — one
/// `recvmsg` is one bundle or an error. Every received right is
/// wrapped as `OwnedFd` immediately upon extraction, so every
/// refusal path below closes automatically (a refused bundle
/// never leaks descriptors). `MSG_CMSG_CLOEXEC` keeps received
/// FDs out of any spawned children; `MSG_CTRUNC`/`MSG_TRUNC`
/// refuse (a fourth FD truncated to three must never read as a
/// valid triple). The caller never sees a partial bundle.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shape/count/truncation
/// violations and `CistellaError::Runtime` on wait/receive
/// failure.
pub fn recv_bundle(sock: &OwnedFd, timeout: Duration) -> Result<(BundleHeader, [OwnedFd; 3])> {
    use nix::poll::{PollFd, PollFlags, poll};
    let mut pollfds = [PollFd::new(sock.as_fd(), PollFlags::POLLIN)];
    let wait = nix::poll::PollTimeout::try_from(timeout)
        .map_err(|_| CistellaError::Runtime("fd wait timeout out of range".to_string()))?;
    let ready = poll(&mut pollfds, wait)
        .map_err(|e| CistellaError::Runtime(format!("poll fd socket: {e}")))?;
    if ready == 0 {
        return Err(CistellaError::Contract(
            "fd bundle wait timed out".to_string(),
        ));
    }
    let (header, fds): (BundleHeader, Vec<OwnedFd>) = {
        let mut data = vec![0u8; MAX_HEADER];
        let mut iov = [IoSliceMut::new(&mut data)];
        let mut cmsgspace = cmsg_space!([std::os::fd::RawFd; MAX_RIGHTS]);
        let received = recvmsg::<UnixAddr>(
            sock.as_raw_fd(),
            &mut iov,
            Some(&mut cmsgspace),
            MsgFlags::MSG_CMSG_CLOEXEC,
        )
        .map_err(|e| CistellaError::Runtime(format!("receive fd bundle: {e}")))?;
        // Own every delivered right FIRST: all checks below drop
        // `OwnedFd`, so every refusal path closes automatically.
        // Truncation is unreachable with a `MAX_RIGHTS` buffer
        // (the kernel caps sends there), but the flag refusal
        // stays behind the wrap for defense in depth.
        let mut fds = Vec::new();
        for cmsg in received
            .cmsgs()
            .map_err(|_| CistellaError::Contract("fd bundle control truncated".to_string()))?
        {
            match cmsg {
                // SAFETY: fresh kernel references from this
                // message, wrapped exactly once each.
                ControlMessageOwned::ScmRights(rights) => fds.extend(
                    rights
                        .into_iter()
                        .map(|raw| unsafe { OwnedFd::from_raw_fd(raw) }),
                ),
                _ => {
                    return Err(CistellaError::Contract(
                        "unexpected fd bundle control message".to_string(),
                    ));
                }
            }
        }
        if received.flags.contains(MsgFlags::MSG_TRUNC)
            || received.flags.contains(MsgFlags::MSG_CTRUNC)
        {
            return Err(CistellaError::Contract("fd bundle truncated".to_string()));
        }
        if received.bytes == 0 {
            return Err(CistellaError::Contract(
                "fd channel closed before bundle".to_string(),
            ));
        }
        let nbytes = received.bytes;
        // `received` is dead past these copies, ending its borrow
        // of `data`: header parsing runs on the plain buffer.
        let header: BundleHeader = serde_json::from_slice(&data[..nbytes]).map_err(|_| {
            CistellaError::Contract("bad fd bundle header: shape violation".to_string())
        })?;
        (header, fds)
    };
    if fds.len() != 3 {
        return Err(CistellaError::Contract(format!(
            "fd bundle must carry exactly 3 descriptors, got {}",
            fds.len()
        )));
    }
    let array: [OwnedFd; 3] = fds
        .into_iter()
        .collect::<Vec<_>>()
        .try_into()
        .expect("exactly 3 fds checked above");
    Ok((header, array))
}
