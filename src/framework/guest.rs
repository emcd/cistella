//! Guest process host: spawn, speak, kill, reap, drain (task 2.1/F4).
//!
//! Split from `protocol` at the supervision seam (file-size limit):
//! the wire exchange lives in [`super::protocol`], process ownership
//! here. Re-exported through `protocol` so spawn paths (`GuestHost`)
//! keep one import root.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::error::{CistellaError, Result};
use crate::framework::protocol::{Exchange, STDERR_CAP, protocol_error};

/// Budget for post-SIGKILL group-extinction settle.
const KILL_SETTLE: Duration = Duration::from_secs(5);

/// Budget for post-kill pipe-EOF verification (FD-holder detection).
const PIPE_EOF_BUDGET: Duration = Duration::from_secs(3);

/// True when no process group survives (signal-0 pole, ESRCH).
///
/// Signal 0 delivers nothing: it only asks the kernel whether the
/// group exists. EPERM (foreign-owned group) also reads as gone:
/// the framework cannot enforce on a group it lacks permission for,
/// so the wait returns rather than blocking on a budget it cannot
/// act on. A recycled PGID could theoretically false-negative, but
/// the pole runs inside a bounded window the framework owns.
fn group_gone(group: Pid) -> bool {
    kill(group, None).is_err()
}

/// Stderr drain accounting (content discarded, counts only).
#[derive(Debug, Clone, Copy)]
pub struct StderrDrain {
    /// Total stderr bytes drained.
    pub bytes: u64,
    /// True when output exceeded [`STDERR_CAP`].
    pub truncated: bool,
}
/// Guest process host: spawn, speak, kill, reap, drain.
///
/// Owns the whole guest lifetime: pinned executable (absolute path
/// required, never PATH-searched), own process group (descendants
/// that fork, change groups, retain FDs, or fill pipes die with the
/// group), concurrent bounded stderr drain (content discarded, counts
/// only), and reverse-order shutdown with residue-dominated
/// reporting. Dead guests arrive as typed write errors, never
/// signals: SIGPIPE is masked per write on the calling thread, so
/// concurrent guests coexist with no shared disposition state.
pub struct GuestHost<R: Read + AsFd, W: Write + AsFd> {
    exchange: Exchange<R, W>,
    child: Child,
    stderr_outcome: mpsc::Receiver<StderrDrain>,
    deadlines: crate::framework::contract::Deadlines,
    /// sha256 hex of the opened executable object (baseline identity).
    executable_digest: String,
    shut: bool,
}

/// Opens an executable and pins it to the open file description.
///
/// Returns the file plus the sha256 hex of its bytes. The caller
/// execs `/proc/self/fd/N`, so path replacement, symlink swaps, or
/// parent-directory games between identity and exec cannot redirect
/// the spawn: identity and execution share one open description.
///
/// # Errors
///
/// Returns `CistellaError::Protocol` on open or read failure.
fn pin_executable(executable: &Path) -> Result<(std::fs::File, String)> {
    use sha2::{Digest, Sha256};
    let file =
        std::fs::File::open(executable).map_err(|e| protocol_error(format!("open guest: {e}")))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut &file, &mut hasher)
        .map_err(|e| protocol_error(format!("digest guest: {e}")))?;
    Ok((file, hex::encode(hasher.finalize())))
}

impl GuestHost<ChildStdout, ChildStdin> {
    /// Spawns a pinned guest executable and opens the exchange.
    ///
    /// The executable must be an absolute path: helpers are
    /// discovered and pinned by the framework, never PATH-searched.
    /// Beyond the path check, spawn binds to the OPENED executable
    /// object: the file is opened, digested, and exec'd via
    /// `/proc/self/fd/N`, so replace/symlink TOCTOU between identity
    /// and exec is structurally impossible. The recorded digest joins
    /// the plan baseline binding for gate revalidation.
    /// Stderr drains on a background thread from spawn (content
    /// discarded immediately); the child leads its own process group.
    ///
    /// Concurrent guests coexist: SIGPIPE is masked per write on the
    /// calling thread (never process-global), so no shared
    /// disposition save/restore exists to corrupt.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on a relative executable,
    /// open/digest failure, or spawn failure.
    pub fn spawn(
        executable: &Path,
        args: &[String],
        deadlines: crate::framework::contract::Deadlines,
    ) -> Result<Self> {
        if !executable.is_absolute() {
            return Err(protocol_error(format!(
                "guest executable must be absolute, never PATH-searched: {}",
                executable.display()
            )));
        }
        let (file, digest) = pin_executable(executable)?;
        // The fd number is stable (we hold the description open).
        // CLOEXEC stays set in the parent: the CHILD clears it on
        // its own inherited copy in pre_exec, so concurrent spawns
        // never observe an inheritable window.
        let exec_fd = file.as_fd().as_raw_fd();
        let exec_path = format!("/proc/self/fd/{exec_fd}");
        let mut child = unsafe {
            Command::new(&exec_path)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .pre_exec(move || {
                    nix::fcntl::fcntl(
                        exec_fd,
                        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()),
                    )
                    .map_err(std::io::Error::other)?;
                    nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0))
                        .map_err(std::io::Error::other)
                })
                .spawn()
        }
        .map_err(|e| protocol_error(format!("guest spawn: {e}")))?;
        // Our description served its pinning purpose; the child holds
        // its own reference past exec.
        drop(file);
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            sender.send(drain_stderr(stderr)).expect("drain report");
        });
        Ok(Self {
            exchange: Exchange::new(stdout, stdin),
            child,
            stderr_outcome: receiver,
            deadlines,
            executable_digest: digest,
            shut: false,
        })
    }

    /// Child pid (observability for tests and conformance).
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// sha256 hex of the opened executable object.
    ///
    /// Joins the plan baseline binding so gate revalidation detects
    /// executable replacement between plan and apply.
    #[must_use]
    pub fn executable_digest(&self) -> &str {
        &self.executable_digest
    }

    /// Mutable frame exchange (hello, requests, streams).
    pub fn exchange_mut(&mut self) -> &mut Exchange<ChildStdout, ChildStdin> {
        &mut self.exchange
    }
}

impl<R: Read + AsFd, W: Write + AsFd> GuestHost<R, W> {
    /// Kills the process group (SIGTERM, grace, SIGKILL) and reaps.
    ///
    /// The group is signalled EVEN when the leader already exited:
    /// a script that spawned sleepers and quit leaves its group
    /// behind, and only an unconditional signal reaches them. Stale
    /// group IDs are harmless (ESRCH ignored) because the very next
    /// step verifies pipe EOF — a wrongly-signalled unrelated group
    /// cannot fake our pipes closed... and signalling uses the
    /// leader's PGID, which the kernel holds stable while any member
    /// survives. Descendants that changed process groups escape the
    /// signal and are caught instead by pipe-EOF verification (a
    /// survivor retaining protocol FDs blocks EOF and reports
    /// residue).
    ///
    /// Trusted-helper boundary (explicit): pinned guests are trusted
    /// code whose DATA is untrusted (framework-lifecycle trust
    /// boundary). Supervision defeats accidental residue — crashes,
    /// hangs, FD leaks — with verification. A guest that actively
    /// escapes its process group AND closes every protocol FD is
    /// outside enforcement; that combination requires deliberate
    /// evasion by code the framework already trusts.
    ///
    /// Completion means leader reaped AND group extinct (signal-0
    /// pole): a sane in-group descendant ignoring SIGTERM but closing
    /// its FDs still counts as residue until SIGKILL clears it. Only
    /// group-escapees are outside — and only by operator-approved
    /// narrowing, pinned by conformance.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` on reap failure or a group
    /// surviving SIGKILL past settle.
    fn kill_group(&mut self) -> Result<()> {
        let group = Pid::from_raw(-(self.child.id() as i32));
        // Unconditional: the prior alive-check skipped the signal
        // when the leader already exited — a bug, not an
        // optimization, because the group still holds descendants
        // that need reaping. ESRCH (stale group) is ignored;
        // pipe-EOF verification owns the proof.
        let _ = kill(group, Signal::SIGTERM);
        let grace = Instant::now() + self.deadlines.terminate_grace;
        let mut reaped = false;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => reaped = true,
                Ok(None) => {}
                Err(e) => return Err(protocol_error(format!("reap: {e}"))),
            }
            if reaped && group_gone(group) {
                return Ok(());
            }
            if Instant::now() >= grace {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = kill(group, Signal::SIGKILL);
        if !reaped {
            self.child
                .wait()
                .map_err(|e| protocol_error(format!("reap after kill: {e}")))?;
        }
        let settle = Instant::now() + KILL_SETTLE;
        while !group_gone(group) {
            if Instant::now() >= settle {
                return Err(protocol_error(
                    "residue: process group survives SIGKILL past settle",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Reverse-order shutdown: close stdin, kill group, reap, verify
    /// pipe EOF (no descendant retains protocol FDs), join the stderr
    /// drain, restore SIGPIPE. Residue dominates: a kill/reap/FD
    /// failure outranks drain or restore failures.
    ///
    /// # Errors
    ///
    /// Returns the residue-class failure when cleanup leaves residue;
    /// drain/restore failures surface only with a clean kill.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.shut {
            return Ok(());
        }
        self.shut = true;
        // Reverse acquisition: stdin pipe, process group, reap, drains.
        self.exchange.close();
        let mut residue: Option<CistellaError> = None;
        if let Err(e) = self.kill_group() {
            residue = Some(e);
        }
        if residue.is_none()
            && let Err(e) = self.verify_pipes_eof()
        {
            residue = Some(e);
        }
        let drain = self.stderr_outcome.recv_timeout(Duration::from_secs(5));
        if residue.is_none()
            && let Err(e) = drain
                .map(|_| ())
                .map_err(|_| protocol_error("stderr drain hung"))
        {
            residue = Some(e);
        }
        if let Some(error) = residue {
            return Err(error);
        }
        Ok(())
    }

    /// Verifies no descendant retains protocol stdout: drains to EOF
    /// on a bounded budget after the kill.
    ///
    /// A survivor holding the write end open blocks EOF and reports
    /// residue (its bytes are discarded; only the blockage matters).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Protocol` when EOF does not arrive in
    /// budget (FD-holder residue) or the drain fails.
    fn verify_pipes_eof(&mut self) -> Result<()> {
        self.exchange
            .drain_reader_to_eof(Instant::now() + PIPE_EOF_BUDGET)
    }
}

impl<R: Read + AsFd, W: Write + AsFd> Drop for GuestHost<R, W> {
    /// Best-effort shutdown: early `?` or unwind must never leave a
    /// peer running. Errors have nowhere to go in `Drop`; explicit
    /// `shutdown` remains the reporting path.
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Reads stderr to EOF, discarding content, counting bytes.
fn drain_stderr(mut stderr: ChildStderr) -> StderrDrain {
    let mut bytes = 0u64;
    let mut chunk = [0u8; 8192];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes += n as u64,
            Err(_) => break,
        }
    }
    StderrDrain {
        bytes,
        truncated: bytes > STDERR_CAP,
    }
}
