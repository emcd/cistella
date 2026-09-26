//! Isolator trait: the backend-neutral lifecycle interface.
//!
//! The framework owns sequencing; isolators own runtime mechanics.
//! `create` / `initiate` / `execute_launch` / `await_result` /
//! `inspect` / `state` / `terminate` / `remove` are distinct external
//! operations with typed handles, reconciliation keys, and
//! idempotent-teardown semantics (removing an absent unit succeeds).
//! The Podman backend (`crate::isolators::podman`) is the first
//! implementor; the deterministic protocol peer (task 3.1) proves
//! boundary behavior against these same signatures.
//!
//! Render-level wart (task 2.2 refines it): `CreateSpec` carries
//! pre-rendered volume/env/label vectors because 1.2 moves mechanics
//! verbatim for zero behavior change. The prepare transaction will
//! replace these with typed contributions; the trait shape already
//! expects that (spec/image/labels stay structured).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::framework::contract::{
    CancelFlag, Capability, ExecutionHandle, LifecycleState, ReconciliationKey, UnitHandle,
};
use crate::session::Session;

/// Capabilities the active isolator declares.
///
/// The framework refuses at merge time any contribution requiring a
/// capability the isolator did not declare.
#[derive(Debug, Clone)]
pub struct IsolatorCapabilities {
    /// Human-readable backend name (e.g. `podman-quadlet`).
    pub backend: &'static str,
    /// Contribution types this backend can realize.
    pub supports: Vec<Capability>,
}

impl IsolatorCapabilities {
    /// True when the backend realizes this contribution type.
    #[must_use]
    pub fn supports(&self, capability: Capability) -> bool {
        self.supports.contains(&capability)
    }
}

/// Creation input: resolved session plus rendered host inputs.
///
/// `volumes` is the flat `[flag, value, ...]` list (`--volume` /
/// `--tmpfs` pairs); `env` is `KEY=value` strings; `labels` is the
/// merged generic label set (driver-owned `cistella.*` added by the
/// backend from the session).
///
/// Serde carries specs across the isolator wire; the framework
/// builds them, the guest executes them, and neither revalidates
/// the other's construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSpec {
    /// Resolved session (identity, image, command, digests).
    pub session: Session,
    /// Rendered volume flags.
    pub volumes: Vec<String>,
    /// Rendered environment assignments.
    pub env: Vec<String>,
    /// Merged generic labels (CLI wins over profile).
    pub labels: Vec<(String, String)>,
}

/// Standard-input/output binding for launched executions.
///
/// In-memory API only (never serialized: file descriptors ride the
/// ancillary-fd channel, not JSON). `Inherit` wires process stdio
/// — correct only for in-process conduct, where process stdio IS
/// the session PTY; the external wire path never offers it (there
/// is no field to carry it, so the confusion is unexpressible).
/// `HeldFiles` wires explicitly passed descriptors, received over
/// the fd channel with header binding and owned exactly once.
#[derive(Debug)]
pub enum StdioBinding {
    /// Inherit the caller's stdio (in-process conduct only).
    Inherit,
    /// Use explicitly held descriptors (external guests only).
    HeldFiles {
        /// Harness standard input.
        stdin: std::os::fd::OwnedFd,
        /// Harness standard output.
        stdout: std::os::fd::OwnedFd,
        /// Harness standard error.
        stderr: std::os::fd::OwnedFd,
        /// Conductor identity (pid, foreground pgid) captured at
        /// launch against the host_pid recorded at guest startup,
        /// if available. The backend joins the foreground pgid
        /// for terminal sessions (see `foreground_join`);
        /// pre_exec re-verifies both halves before trusting them.
        conductor: Option<(u32, u32)>,
    },
}

/// Foreground-join decision for a harness launch: join only with a
/// verified conductor pgid on a terminal session; refuse a TTY
/// launch without one (a silent background launch would stall with
/// SIGTTIN instead); never move piped sessions (no terminal
/// semantics). In-process `Inherit` launches never consult this
/// (already in the caller's group).
#[derive(Debug, PartialEq, Eq)]
pub enum ForegroundJoin {
    /// Join this conductor (pid, foreground pgid) in the spawned child.
    Join((u32, u32)),
    /// Refuse the launch typed: no safe foreground exists.
    Refuse,
    /// Stay in the spawner's group.
    Stay,
}

/// Decides the foreground join from session shape: terminal plus
/// verified conductor joins, terminal without conductor refuses,
/// piped stays. Pure decision seam — the truth table pins
/// directly instead of through a live PTY.
#[must_use]
pub fn foreground_join(tty: bool, conductor: Option<(u32, u32)>) -> ForegroundJoin {
    match (tty, conductor) {
        (true, Some(identity)) => ForegroundJoin::Join(identity),
        (true, None) => ForegroundJoin::Refuse,
        (false, _) => ForegroundJoin::Stay,
    }
}

/// Maps a parentage observation to verified conductor identity
/// (pid, pgid): the parent must still be the conductor recorded at
/// guest startup (a subreaper adoption fails the equality even when
/// it is not init), and only then does a looked-up pgid pass
/// through with its pid. A failed lookup maps to None here; the
/// TTY branch turns that into [`ForegroundJoin::Refuse`]
/// downstream.
#[must_use]
pub fn verified_conductor_pgid(parent: u32, host: u32, pgid: Option<u32>) -> Option<(u32, u32)> {
    if parent == host {
        pgid.map(|pgid| (parent, pgid))
    } else {
        None
    }
}

/// True when the session input is a terminal: the harness must run
/// in the caller's (foreground) process group for terminal I/O to
/// flow instead of stopping with SIGTTIN/SIGTTOU.
#[must_use]
pub fn is_session_tty<Fd: std::os::fd::AsFd + std::os::fd::AsRawFd>(fd: &Fd) -> bool {
    nix::unistd::isatty(fd.as_raw_fd()).unwrap_or(false)
}

/// Initiate attestation: proof the unit started.
///
/// `pidns_proof` is the container init's PID-namespace identifier
/// (Linux `pid:[inode]` form), proving the unit runs isolated;
/// backends that cannot prove it fail initiate rather than
/// attest blindly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedAttestation {
    /// Unit identity (container name).
    pub unit_identity: String,
    /// PID-namespace proof for the running unit.
    pub pidns_proof: String,
    /// Initiate completed and the unit is ready for guest prep.
    pub ready: bool,
}

/// Execution outcome: exit code or 128+signal disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionOutcome {
    /// Harness exited with a status code.
    Exited(i32),
    /// Harness ended by signal (`128 + signum`).
    Signaled(i32),
}

impl ExecutionOutcome {
    /// Process exit code for this outcome.
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Exited(code) => code,
            Self::Signaled(signum) => 128 + signum,
        }
    }
}

/// Read-only unit snapshot (`inspect`: never mutates).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitSnapshot {
    /// Unit identity (container name).
    pub unit_identity: String,
    /// Lifecycle state at inspect time.
    pub lifecycle: LifecycleState,
    /// Systemd `ActiveState` (`active`, `inactive`, `failed`, ...).
    pub active_state: String,
    /// Session id from the `cistella.id` label (empty when absent).
    pub session_id: String,
    /// Image reference from the unit.
    pub image: String,
}

/// Terminate attestation: proof the unit stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedAttestation {
    /// Unit identity (container name).
    pub unit_identity: String,
}

/// Remove attestation: proof the unit is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedAttestation {
    /// Unit identity (container name).
    pub unit_identity: String,
}

/// Backend-neutral isolator lifecycle.
///
/// Locking contract: methods assume the caller holds the
/// creation-window lock wherever the moved mechanics did (create
/// through post-initiate preparation). Teardown entry points used
/// outside the window (`terminate` command, GC) acquire it
/// themselves before delegating. Idempotency: `terminate` and
/// `remove` against an absent unit succeed with attestations, never
/// errors.
pub trait Isolator: Send + Sync {
    /// Declared capabilities for contribution gating.
    fn capabilities(&self) -> IsolatorCapabilities;

    /// Creates the unit (unit file install, scratch creation) and
    /// returns a framework-opaque handle. Records the reconciliation
    /// key so a replacement peer can locate the unit by key when the
    /// handle never arrived.
    ///
    /// # Errors
    ///
    /// Returns on install/IO failure with no partial unit left behind.
    fn create(&self, spec: &CreateSpec, key: &ReconciliationKey) -> Result<UnitHandle>;

    /// Starts the unit; returns the initiate attestation.
    ///
    /// # Errors
    ///
    /// Returns when the unit fails to reach active or the deadline
    /// expires; the caller tears down.
    fn initiate(&self, handle: &UnitHandle, key: &ReconciliationKey) -> Result<StartedAttestation>;

    /// Launches the harness argv without blocking for completion and
    /// returns an awaitable execution handle.
    ///
    /// `workdir` is launch context (the session worktree target);
    /// `None` runs the image default. The wire schema gains it in
    /// task 2.1; the prepare transaction (2.2) owns launch context.
    ///
    /// # Errors
    ///
    /// Returns on spawn failure only; launch never reports harness
    /// outcome (that is `await_result`'s job).
    fn execute_launch(
        &self,
        handle: &UnitHandle,
        argv: &[String],
        workdir: Option<&str>,
        stdio: StdioBinding,
        key: &ReconciliationKey,
    ) -> Result<ExecutionHandle>;

    /// Awaits one launched execution to completion, honoring
    /// cancellation. Cancelling detaches without killing; the outcome
    /// stays redeemable until `remove`.
    ///
    /// # Errors
    ///
    /// Returns on wait failure; harness failure arrives as
    /// [`ExecutionOutcome`], never an error.
    fn await_result(
        &self,
        execution: &ExecutionHandle,
        cancel: &CancelFlag,
    ) -> Result<ExecutionOutcome>;

    /// Returns a rich read-only snapshot; never mutates.
    ///
    /// # Errors
    ///
    /// Returns when the unit cannot be inspected and absence cannot
    /// be established (fail closed: never read failure as absence).
    fn inspect(&self, handle: &UnitHandle) -> Result<UnitSnapshot>;

    /// Returns the lifecycle-state enum only.
    ///
    /// # Errors
    ///
    /// Returns on backend query failure.
    fn state(&self, handle: &UnitHandle) -> Result<LifecycleState>;

    /// Stops the unit with SIGTERM grace then SIGKILL semantics and
    /// settles it; absent units succeed.
    ///
    /// # Errors
    ///
    /// Returns on stop failure with residue remaining.
    fn terminate(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        key: &ReconciliationKey,
    ) -> Result<StoppedAttestation>;

    /// Removes the unit file, reloads, resets failure state, removes
    /// scratch; absent units succeed.
    ///
    /// # Errors
    ///
    /// Returns on removal failure with residue remaining.
    fn remove(&self, handle: &UnitHandle, key: &ReconciliationKey) -> Result<RemovedAttestation>;

    /// Locates a unit by reconciliation key (replacement-peer
    /// recovery independent of any returned handle).
    ///
    /// Fail-closed: any authoritative query error refuses instead of
    /// reporting absence (an uncertain scan must never greenlight a
    /// duplicate install). `Ok(None)` after clean queries genuinely
    /// means absent.
    ///
    /// # Errors
    ///
    /// Returns on backend query failure.
    fn locate(&self, key: &ReconciliationKey) -> Result<Option<UnitHandle>>;

    /// Converges any unit to applied-or-clean: inspects, terminates
    /// when present, removes always. The default implementation is
    /// backend-neutral; backends override only for mechanism reasons.
    ///
    /// # Errors
    ///
    /// Returns when termination or removal leaves residue.
    fn converge_clean(
        &self,
        handle: &UnitHandle,
        grace: Duration,
        key: &ReconciliationKey,
    ) -> Result<()> {
        match self.state(handle)? {
            LifecycleState::Absent => Ok(()),
            _ => {
                self.terminate(handle, grace, key)?;
                self.remove(handle, key)?;
                Ok(())
            }
        }
    }
}
