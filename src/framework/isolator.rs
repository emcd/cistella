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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioBinding {
    /// Inherit the caller's stdio (the session PTY slave).
    ///
    /// The only binding today; the enum stays open so PTY-slave,
    /// captured, or null bindings land without trait churn.
    Inherit,
}

/// Initiate attestation: proof the unit started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedAttestation {
    /// Unit identity (container name).
    pub unit_identity: String,
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
