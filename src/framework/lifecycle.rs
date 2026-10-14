//! Session lifecycle handles and reconciliation identity.
//!
//! Lifecycle states, opaque handles, reconciliation keys, and the
//! baseline binding with gate-held revalidation. Split from
//! `contract` at the file-size limit; the merge pipeline stays
//! there.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};

use serde::{Deserialize, Serialize};

use crate::error::{CistellaError, Result};
use crate::session::mint_session_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleState {
    /// Created but not initiated.
    Created,
    /// Initiated/started but not executing.
    Initiated,
    /// Harness executing.
    Executing,
    /// Stopped but present (results replayable until remove).
    Stopped,
    /// Absent (teardown of absent succeeds).
    Absent,
}

/// Opaque framework-issued unit handle.
///
/// Never a path, never guest-chosen; only the framework mints these
/// (construction is crate-internal so guests cannot forge them).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnitHandle(String);

impl UnitHandle {
    /// Mints a framework-issued handle.
    ///
    /// Backends mint at `create` (and `adopt`); callers and guests
    /// treat handles as opaque and never construct them to name a
    /// unit they did not create.
    pub fn mint() -> Self {
        Self(mint_session_id())
    }

    /// Opaque identifier for wire envelopes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque framework-issued execution handle.
///
/// Redeemed by a later `await_result`; results stay replayable until
/// `remove`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionHandle(String);

impl ExecutionHandle {
    /// Mints a framework-issued handle.
    ///
    /// Backends mint at `execute_launch`; callers treat handles as
    /// opaque and never construct them to redeem an execution they
    /// did not launch.
    pub fn mint() -> Self {
        Self(mint_session_id())
    }

    /// Opaque identifier for wire envelopes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Client-supplied reconciliation key: durable resource identity.
///
/// Opaque string (format unconstrained); one key per resource
/// attempt, reused across retries of the same attempt. A replacement
/// peer presents the key to locate an uncertain resource independent
/// of any returned handle. A request-correlation ID must never serve
/// as a reconciliation key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReconciliationKey(String);

impl ReconciliationKey {
    /// Generates a fresh unique key for a new resource attempt.
    #[must_use]
    pub fn generate() -> Self {
        Self(mint_session_id())
    }

    /// Reuses the caller-supplied key for a retry of the same attempt.
    #[must_use]
    pub fn reuse(key: &str) -> Self {
        Self(key.to_string())
    }

    /// Key text for wire envelopes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Pinned extension executable identity for baseline binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionIdentity {
    /// Framework-resolved executable path (never PATH-searched).
    pub executable: String,
    /// Advertised version string.
    pub version: String,
    /// Digest of the opened/stable executable object.
    pub digest: String,
}

/// One named resource assumption covered by the baseline binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assumption {
    /// Assumption name (e.g. `profile`, `unit-image`, `mount-source`).
    pub key: String,
    /// Digest of the assumed state at plan time.
    pub digest: String,
}

/// Baseline binding carried by every plan.
///
/// Covers at minimum the resolved profile/session digest, extension
/// executable identities/versions, and relevant resource-state
/// digests. After the resource gate is acquired and before apply,
/// the framework revalidates assumptions against live state; drift
/// fails the plan before mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineBinding {
    /// sha256 hex digest of the resolved profile/session input.
    pub profile_digest: String,
    /// Pinned extension identities.
    pub extensions: Vec<ExtensionIdentity>,
    /// Resource-state assumptions to revalidate at the gate.
    pub assumptions: Vec<Assumption>,
}

impl BaselineBinding {
    /// Revalidates assumptions against live `(key, digest)` state.
    ///
    /// Pure: compares digests only. First drifted assumption fails
    /// with a typed error naming the key, never the state.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` naming the first drifted or
    /// missing assumption.
    pub fn revalidate(&self, live: &HashMap<String, String>) -> Result<()> {
        for assumption in &self.assumptions {
            match live.get(&assumption.key) {
                Some(digest) if digest == &assumption.digest => {}
                _ => {
                    return Err(CistellaError::Contract(format!(
                        "plan assumption drifted at gate: {}",
                        assumption.key
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Framework-owned cancellation flag with signal provenance.
///
/// Conduct-level traps record the signal number here; `await_result`
/// polls it instead of touching process globals, so fakes and tests
/// drive cancellation without signals. `0` means not cancelled.
#[derive(Debug, Default)]
pub struct CancelFlag {
    signum: AtomicI32,
}

impl CancelFlag {
    /// Fresh uncancelled flag (const so process-wide statics can hold one).
    ///
    /// Public so external guest binaries (separate `--bin` targets in
    /// this crate) can own cancellation state; in-process callers
    /// keep using the shared conduct/static flags.
    pub const fn new() -> Self {
        Self {
            signum: AtomicI32::new(0),
        }
    }

    /// Cancels with the given signal number (e.g. `1`/`15`).
    pub fn cancel_with(&self, signum: i32) {
        self.signum.store(signum, Ordering::SeqCst);
    }

    /// True once cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.signum.load(Ordering::SeqCst) != 0
    }

    /// Pending signal number, if cancelled.
    #[must_use]
    pub fn signum(&self) -> Option<i32> {
        match self.signum.load(Ordering::SeqCst) {
            0 => None,
            signum => Some(signum),
        }
    }
}
