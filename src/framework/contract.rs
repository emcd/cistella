//! Framework lifecycle/capability contract.
//!
//! The spine, contribution schemas, merge rules, and deadline ownership
//! that every isolator and extension obeys. Written before any
//! extraction so the abstraction is defined by the contract, never by
//! current Podman call sites (`framework-isolators-extensions` task
//! 1.1).
//!
//! The contract is three parts: [`Phase`] orders the nine spine phases;
//! [`PreparePlan`] plus [`merge_prepare`] define the single typed
//! prepare transaction and its atomic central merge; [`Deadlines`]
//! keeps every control-plane bound framework-owned. Identity types
//! ([`UnitHandle`], [`ExecutionHandle`], [`ReconciliationKey`],
//! [`BaselineBinding`], [`LifecycleState`]) pin the reconciliation and
//! idempotent-teardown semantics the isolator contract builds on.
//!
//! Planning is side-effect free by construction: [`PreparePlan`] and
//! [`MergedPlan`] are plain data, and [`merge_prepare`] is a pure
//! function. Nothing here touches units, mounts, files, or containers.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{CistellaError, Result};
use crate::mount::validate_mounts;
pub use crate::mount::{MountMode, MountTriple};
use crate::profile::validate_env_name;
use crate::session::mint_session_id;

/// Spine phases in execution order.
///
/// Phases 1, 2, and 5 are planning (side-effect free); phases 2
/// (apply), 3, 4, 6, 7, and 9 mutate only after acquiring their gate.
/// Phase 8 (await) is intentionally unbounded: the harness lifetime is
/// never capped, only teardown ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    /// Pure planning: resolve input, negotiate capabilities, collect
    /// extension plans, merge centrally, evaluate policy.
    Planning,
    /// Gated host pre-create apply (resource revalidation first).
    ///
    /// Marked planning-pure: this phase validates against live state
    /// and never mutates; mutation happens only at the gated apply
    /// steps that follow validation.
    GateHostPreCreate,
    /// Isolator create.
    Create,
    /// Isolator initiate/start.
    Initiate,
    /// Post-initiate guest probe and preparation (mountpoint
    /// preparation maps here).
    PostInitiatePrepare,
    /// Guest restriction/wrapper establishment.
    GuestRestriction,
    /// Bounded execute launch (launch never blocks for completion).
    ExecuteLaunch,
    /// Session-lifetime await/result (uncapped by design).
    AwaitResult,
    /// Reverse-order cleanup/teardown with residue-dominated reporting.
    Teardown,
}

impl Phase {
    /// Spine position, 0 through 8.
    #[must_use]
    pub fn index(self) -> u8 {
        match self {
            Self::Planning => 0,
            Self::GateHostPreCreate => 1,
            Self::Create => 2,
            Self::Initiate => 3,
            Self::PostInitiatePrepare => 4,
            Self::GuestRestriction => 5,
            Self::ExecuteLaunch => 6,
            Self::AwaitResult => 7,
            Self::Teardown => 8,
        }
    }

    /// True for phases that must not mutate host or container state.
    #[must_use]
    pub fn is_planning(self) -> bool {
        matches!(
            self,
            Self::Planning | Self::GateHostPreCreate | Self::PostInitiatePrepare
        )
    }
}

/// Contribution types a guest may return.
///
/// Capability advertisement declares which types each guest may
/// return; the merge refuses any non-empty set whose type was not
/// advertised. Empty sets contribute nothing and need no capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Environment contribution set.
    Environment,
    /// Mount contribution set.
    Mounts,
    /// Policy claims.
    PolicyClaims,
    /// Guest-hook requests.
    GuestHooks,
    /// Credential contributions (strict handle variants only).
    Credentials,
}

/// Advertised capability set for one guest.
#[derive(Debug, Clone, Default)]
pub struct CapabilitySet {
    inner: HashSet<Capability>,
}

impl CapabilitySet {
    /// Advertises exactly these capabilities.
    #[must_use]
    pub fn new(capabilities: &[Capability]) -> Self {
        Self {
            inner: capabilities.iter().copied().collect(),
        }
    }

    /// True when the guest advertised this contribution type.
    #[must_use]
    pub fn allows(&self, capability: Capability) -> bool {
        self.inner.contains(&capability)
    }
}

/// Where a contribution came from.
///
/// Provenance drives policy scoping: `on-extensions` cells match
/// extension contributions only, never profile values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    /// Declared by the profile under conduct.
    Profile,
    /// Contributed by the named extension guest.
    Extension(String),
}

/// One environment contribution: exact name plus opaque value.
///
/// Values are never rendered in diagnostics; refusals name the
/// variable only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvContribution {
    /// Exact variable name (`[A-Z_][A-Z0-9_]*`).
    pub name: String,
    /// Opaque value (control characters refused at merge).
    pub value: String,
    /// Contribution source for policy scoping.
    pub provenance: Provenance,
}

/// One mount contribution: an allowlist triple plus provenance.
#[derive(Debug, Clone)]
pub struct MountContribution {
    /// Mount triple validated by the existing mount rules.
    pub triple: MountTriple,
    /// Contribution source for policy scoping.
    pub provenance: Provenance,
}

/// Policy severity: acknowledgement-escapable or absolute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Refuses unless an exact-name acknowledgement exists (user
    /// policy only; compiled defaults are suppressible only).
    Suppressible,
    /// Refuses unconditionally; site authority only.
    Inviolable,
}

/// Policy scope: every value or extension contributions only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Matches any provenance.
    Universal,
    /// Matches extension contributions only.
    OnExtensions,
}

/// One policy claim inside a prepare transaction.
///
/// Shape-checked at merge (typed severity/scope, non-empty pattern);
/// lattice evaluation against site/user/defaults is the prepare
/// transaction's work (task 2.2), not the merge's. Claims are scoped
/// to their own transaction and contributions: overreach refuses the
/// whole transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyClaim {
    /// Name or pattern the claim constrains (regex compiled at
    /// evaluation; non-empty required at merge).
    pub pattern: String,
    /// Claimed severity (tighten-only against applicable rules).
    pub severity: Severity,
    /// Claimed scope.
    pub scope: Scope,
}

/// One guest-hook request: pre-exec wrapper presence.
///
/// The extension never selects channel numbers: ordering is an
/// explicit sequence position, and the framework assigns the
/// diagnostics channel in the accepted-plan response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestHookRequest {
    /// Composition position (framework-declared sequence; duplicate
    /// positions across hooks refuse at merge).
    pub order: u32,
    /// Wrapper argv prefix; harness argv appends verbatim after it.
    pub argv_prefix: Vec<String>,
    /// Guest-context capability probe operation name.
    pub probe_op: String,
}

/// The single prepare transaction: separately-typed contribution sets.
///
/// At most one per session per extension. Untrusted input at every
/// step; [`merge_prepare`] validates and merges atomically.
#[derive(Debug, Clone, Default)]
pub struct PreparePlan {
    /// Environment contributions.
    pub environment: Vec<EnvContribution>,
    /// Mount contributions.
    pub mounts: Vec<MountContribution>,
    /// Policy claims scoped to this transaction.
    pub policy_claims: Vec<PolicyClaim>,
    /// Guest-hook requests.
    pub guest_hooks: Vec<GuestHookRequest>,
}

/// Centrally merged plan: validated, spine-ordered, ready to gate.
///
/// Environment keeps first-seen spine order; mounts keep plan order
/// after topology validation; hooks sort by declared order.
#[derive(Debug, Clone)]
pub struct MergedPlan {
    /// Merged `(name, value)` pairs in spine order.
    pub environment: Vec<(String, String)>,
    /// Merged mount triples in plan order.
    pub mounts: Vec<MountTriple>,
    /// Shape-checked policy claims for lattice evaluation.
    pub policy_claims: Vec<PolicyClaim>,
    /// Guest hooks in composition order.
    pub guest_hooks: Vec<GuestHookRequest>,
}

/// Framework-owned baseline inputs for central merge.
///
/// The extension's sets never merge in isolation: environment
/// contributions collide against the full destination namespace
/// (profile assignments, acceptances, driver-injected names like
/// `HOME`), and mount contributions validate jointly with the full
/// emitted set (profile/CLI/session/scratch/credential triples).
#[derive(Debug, Clone, Default)]
pub struct MergeContext {
    /// Container home for topology validation.
    pub container_home: String,
    /// Reserved env names (profile + driver namespace).
    pub reserved_env: HashSet<String>,
    /// Already-emitted mount triples (profile/CLI/session/...).
    pub occupied_mounts: Vec<MountTriple>,
}

impl MergeContext {
    /// Baseline inputs for central merge.
    pub fn new(
        container_home: &str,
        reserved_env: HashSet<String>,
        occupied_mounts: Vec<MountTriple>,
    ) -> Self {
        Self {
            container_home: container_home.to_string(),
            reserved_env,
            occupied_mounts,
        }
    }

    /// Standalone merge with no baseline.
    ///
    /// Tests-only: production callers (conduct wiring) must fill the
    /// full destination namespace and emitted mount set — an empty
    /// context there would silently skip collision checks.
    pub fn empty(container_home: &str) -> Self {
        Self {
            container_home: container_home.to_string(),
            reserved_env: HashSet::new(),
            occupied_mounts: Vec::new(),
        }
    }
}

/// Validates and centrally merges one prepare transaction.
///
/// Atomic: every check runs before any output is built, so the
/// result is either a whole merged plan or a typed refusal — partial
/// application never occurs. In order: capability advertisement gate
/// (non-empty sets need their type advertised), per-type validation
/// with existing rules (env-name grammar, value gate, mount
/// topology), destination collisions (extension names against the
/// reserved namespace, extension triples jointly with the occupied
/// set), duplicate refusal (env names, hook orders), claim shape
/// checks. Diagnostics name variables, never values.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on undeclared contribution
/// types, invalid names/values, mount violations, destination
/// collisions, duplicates, or malformed claims.
pub fn merge_prepare(
    plan: PreparePlan,
    advertised: &CapabilitySet,
    context: &MergeContext,
) -> Result<MergedPlan> {
    gate_capabilities(&plan, advertised)?;
    let environment = merge_environment(&plan.environment, &context.reserved_env)?;
    let mut triples: Vec<MountTriple> = context.occupied_mounts.clone();
    triples.extend(
        plan.mounts
            .iter()
            .map(|contribution| contribution.triple.clone()),
    );
    validate_mounts(&triples, &context.container_home).map_err(contract_error)?;
    let contributed: Vec<MountTriple> = triples[context.occupied_mounts.len()..].to_vec();
    let policy_claims = check_claims(&plan.policy_claims)?;
    let guest_hooks = order_hooks(&plan.guest_hooks)?;
    Ok(MergedPlan {
        environment,
        mounts: contributed,
        policy_claims,
        guest_hooks,
    })
}

fn contract_error(error: CistellaError) -> CistellaError {
    CistellaError::Contract(error.to_string())
}

fn gate_capabilities(plan: &PreparePlan, advertised: &CapabilitySet) -> Result<()> {
    for (nonempty, capability, what) in [
        (
            !plan.environment.is_empty(),
            Capability::Environment,
            "environment",
        ),
        (!plan.mounts.is_empty(), Capability::Mounts, "mounts"),
        (
            !plan.policy_claims.is_empty(),
            Capability::PolicyClaims,
            "policy-claims",
        ),
        (
            !plan.guest_hooks.is_empty(),
            Capability::GuestHooks,
            "guest-hooks",
        ),
    ] {
        if nonempty && !advertised.allows(capability) {
            return Err(CistellaError::Contract(format!(
                "guest returned unadvertised contribution type: {what}"
            )));
        }
    }
    Ok(())
}

fn merge_environment(
    contributions: &[EnvContribution],
    reserved: &HashSet<String>,
) -> Result<Vec<(String, String)>> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut merged = Vec::with_capacity(contributions.len());
    for contribution in contributions {
        validate_env_name(&contribution.name, "contribution").map_err(contract_error)?;
        if contribution.value.contains('\n')
            || contribution.value.contains('\r')
            || contribution.value.contains('\0')
        {
            return Err(CistellaError::Contract(format!(
                "contribution value must not contain control characters: {}",
                contribution.name
            )));
        }
        if reserved.contains(&contribution.name) {
            return Err(CistellaError::Contract(format!(
                "contribution collides with reserved name: {}",
                contribution.name
            )));
        }
        if !seen.insert(contribution.name.as_str()) {
            return Err(CistellaError::Contract(format!(
                "duplicate environment contribution: {}",
                contribution.name
            )));
        }
        merged.push((contribution.name.clone(), contribution.value.clone()));
    }
    Ok(merged)
}

fn check_claims(claims: &[PolicyClaim]) -> Result<Vec<PolicyClaim>> {
    for claim in claims {
        if claim.pattern.is_empty() {
            return Err(CistellaError::Contract(
                "policy claim pattern must not be empty".to_string(),
            ));
        }
    }
    Ok(claims.to_vec())
}

fn order_hooks(hooks: &[GuestHookRequest]) -> Result<Vec<GuestHookRequest>> {
    let mut seen: HashSet<u32> = HashSet::new();
    for hook in hooks {
        if hook.argv_prefix.is_empty() {
            return Err(CistellaError::Contract(format!(
                "guest hook argv prefix must not be empty: order {}",
                hook.order
            )));
        }
        if hook.probe_op.is_empty() {
            return Err(CistellaError::Contract(format!(
                "guest hook probe op must not be empty: order {}",
                hook.order
            )));
        }
        if !seen.insert(hook.order) {
            return Err(CistellaError::Contract(format!(
                "duplicate guest hook order: {}",
                hook.order
            )));
        }
    }
    let mut ordered = hooks.to_vec();
    ordered.sort_by_key(|hook| hook.order);
    Ok(ordered)
}

/// Isolator lifecycle state.
///
/// `state` returns only this enum; `inspect` returns the rich
/// read-only snapshot. Every transition stays a separate operation.
///
/// Lowercase wire spelling (`created|initiated|...`) matches the
/// isolator-contract schema on both `state` and `inspect` paths —
/// one form, never Debug-derived.
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

/// Framework-owned control-plane deadlines.
///
/// Every bound is framework-chosen: guests never select timeouts.
/// Mechanism (process groups, pidfds, cgroups) and these default
/// magnitudes are implementation choices; the ownership is contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// Hello/negotiation window (short).
    pub hello: Duration,
    /// Plan-collection window per guest (bounded).
    pub plan: Duration,
    /// Apply window per guest (bounded).
    pub apply: Duration,
    /// SIGTERM grace before SIGKILL on teardown/kill.
    pub terminate_grace: Duration,
    /// Frame-completion budget once any response byte arrives
    /// (control-plane framing stays bounded even when the
    /// harness wait itself is uncapped).
    pub frame_completion: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(5),
            plan: Duration::from_secs(30),
            apply: Duration::from_secs(30),
            terminate_grace: Duration::from_secs(10),
            frame_completion: Duration::from_secs(60),
        }
    }
}

/// Control-plane interactions that carry a framework deadline.
///
/// The harness await itself is uncapped and has no entry here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlDeadline {
    /// Hello/negotiation exchange.
    Hello,
    /// Plan collection.
    Plan,
    /// Gated apply.
    Apply,
    /// SIGTERM grace before SIGKILL.
    TerminateGrace,
    /// Frame completion once response bytes arrive.
    FrameCompletion,
}

impl Deadlines {
    /// Looks up the bound for one control-plane interaction.
    #[must_use]
    pub fn for_interaction(self, interaction: ControlDeadline) -> Duration {
        match interaction {
            ControlDeadline::Hello => self.hello,
            ControlDeadline::Plan => self.plan,
            ControlDeadline::Apply => self.apply,
            ControlDeadline::TerminateGrace => self.terminate_grace,
            ControlDeadline::FrameCompletion => self.frame_completion,
        }
    }
}
