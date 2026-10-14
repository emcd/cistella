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

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{CistellaError, Result};
use crate::framework::credentials::AdmittedCredential;
use crate::framework::landlock_compiler::FULL_BASELINE_ROUTES;
use crate::framework::policy::PolicySet;
pub use crate::mount::{MountMode, MountTriple, Placement};
use crate::mount::{canonicalize_container_target, validate_mounts};
use crate::profile::validate_env_name;

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
    /// Permission contribution set.
    Permissions,
    /// Policy claims.
    PolicyClaims,
    /// Guest-hook requests.
    GuestHooks,
    /// Cleanup declarations (minted host paths the framework reaps).
    Cleanup,
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

/// One mount contribution: a modeless placement plus provenance.
///
/// Rights travel separately as permissions; placements never carry
/// modes.
#[derive(Debug, Clone)]
pub struct MountContribution {
    /// Modeless placement validated by the existing mount rules.
    pub placement: Placement,
    /// Contribution source for policy scoping.
    pub provenance: Provenance,
}

pub use crate::framework::permissions::{
    EffectiveGrant, EffectivePermissions, PermissionContribution, Rights, render_placements,
};

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
/// transaction's work, not the merge's. Claims are scoped
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

/// Hook artifact reference: digest-pinned executable blob.
///
/// The extension names the bytes; the framework resolves the
/// registry, pins and hashes the file, and stages a verified copy.
/// The digest binds the staged copy to the hashed bytes — the trust
/// anchor stays the operator-owned install directory, never the
/// extension's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookArtifact {
    /// Artifact kind; exactly `digest-pinned-blob`.
    pub kind: String,
    /// Expected SHA-256 over the registry bytes (64 lowercase hex).
    pub sha256: String,
    /// Registry id plus registry-relative path.
    pub source: HookSource,
}

/// Registry coordinates for a hook artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSource {
    /// Framework-admitted registry id (admission at resolve time).
    pub registry: String,
    /// Registry-relative path (shape-checked at merge, escape-checked
    /// at resolve).
    pub path: String,
}

/// Guest-context capability probe descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookProbe {
    /// Probe operation name.
    pub op: String,
    /// Probe budget in milliseconds (must be nonzero; the
    /// framework-owned deadline owns the ceiling).
    pub timeout_ms: u64,
}

/// One guest-hook request: pre-exec wrapper presence.
///
/// The extension never selects channel numbers: ordering is an
/// explicit sequence position, and the framework assigns the
/// diagnostics channel in the accepted-plan response. The schema is
/// exact per the extension-protocol contract: deviations refuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestHookRequest {
    /// Digest-pinned wrapper artifact reference.
    pub artifact: HookArtifact,
    /// Staging owner; exactly `isolator-staged`.
    pub staging: String,
    /// Composition position (framework-declared sequence; duplicate
    /// positions across hooks refuse at merge).
    pub order: u32,
    /// Wrapper argv prefix; harness argv appends verbatim after it.
    /// The first element names the staged wrapper guest path.
    pub argv_prefix: Vec<String>,
    /// Guest-context capability probe descriptor.
    pub probe: HookProbe,
    /// Probe/apply failure disposition; exactly `fail-pre-exec`.
    pub on_failure: String,
}

/// Admitted hook artifact kind: content-pinned executable blob.
pub const HOOK_ARTIFACT_KIND_BLOB: &str = "digest-pinned-blob";

/// Admitted hook staging owner: the isolator stages pre-initiate.
pub const HOOK_STAGING_ISOLATOR: &str = "isolator-staged";

/// Admitted hook failure disposition: fail before execute.
pub const HOOK_ON_FAILURE_PRE_EXEC: &str = "fail-pre-exec";

/// DoS bound on hook argv length: composition stays reviewable.
/// Per-hook admission (e.g. the Landlock singleton executable)
/// tightens further; this is the structural ceiling for all hooks.
pub const MAX_HOOK_ARGV: usize = 16;

/// The single prepare transaction: separately-typed contribution sets.
///
/// At most one per session per extension. Untrusted input at every
/// step; [`merge_prepare`] validates and merges atomically.
#[derive(Debug, Clone, Default)]
pub struct PreparePlan {
    /// Environment contributions.
    pub environment: Vec<EnvContribution>,
    /// Mount contributions (modeless placements).
    pub mounts: Vec<MountContribution>,
    /// Permission contributions (rights on guest-visible targets).
    pub permissions: Vec<PermissionContribution>,
    /// Policy claims scoped to this transaction.
    pub policy_claims: Vec<PolicyClaim>,
    /// Guest-hook requests.
    pub guest_hooks: Vec<GuestHookRequest>,
    /// Cleanup declarations: absolute host paths the extension
    /// minted and owns (reaped post-convergence, never enumerated
    /// by gc beyond the attributable pattern).
    pub cleanup: Vec<String>,
}

/// Centrally merged plan: validated, spine-ordered, ready to gate.
///
/// Environment keeps first-seen spine order; mounts keep plan order
/// after topology validation; hooks sort by declared order.
#[derive(Debug, Clone)]
pub struct MergedPlan {
    /// Merged `(name, value)` pairs in spine order.
    pub environment: Vec<(String, String)>,
    /// Merged placements in plan order.
    pub placements: Vec<Placement>,
    /// Joined effective permissions (canonical target order).
    pub permissions: Vec<EffectiveGrant>,
    /// Shape-checked policy claims for lattice evaluation.
    pub policy_claims: Vec<PolicyClaim>,
    /// Guest hooks in composition order.
    pub guest_hooks: Vec<GuestHookRequest>,
    /// Cleanup declarations in plan order (absolute paths only).
    pub cleanup: Vec<String>,
}

/// Evaluated plan: merged contributions plus claim diagnostics.
///
/// `diagnostics` records discarded weakenings (name-only); the merged
/// plan is whole or the transaction refused — partial application
/// never occurs. Admitted credential handles ride alongside for the
/// dogfood gate to consume.
#[derive(Debug, Clone)]
pub struct EvaluatedPlan {
    /// Centrally merged, policy-admitted plan.
    pub merged: MergedPlan,
    /// Post-merge effective permissions (driver feed plus joined
    /// contributions): the single source of truth downstream reads.
    pub effective: EffectivePermissions,
    /// Weakening-discard notes from claim partition.
    pub diagnostics: Vec<String>,
    /// Admitted credential handles (kinds + locator classes only).
    pub credentials: Vec<AdmittedCredential>,
}

// Re-exported through `prepare` so existing import roots keep working.

/// Framework-owned baseline inputs for central merge.
///
/// The extension's sets never merge in isolation: environment
/// contributions collide against the full destination namespace
/// (profile assignments, acceptances, driver-injected names like
/// `HOME`), and mount contributions validate jointly with the full
/// emitted set (profile/CLI/session/credential triples).
#[derive(Debug, Clone, Default)]
pub struct MergeContext {
    /// Container home for topology validation.
    pub container_home: String,
    /// Reserved env names (profile + driver namespace).
    pub reserved_env: HashSet<String>,
    /// Already-emitted mount triples (profile/CLI/session/...).
    pub occupied_mounts: Vec<MountTriple>,
    /// Explicit profile `[[permissions]]` as contributions
    /// (profile provenance); joined with guest contributions.
    pub profile_permissions: Vec<PermissionContribution>,
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
            profile_permissions: Vec::new(),
        }
    }

    /// Attaches explicit profile `[[permissions]] contributions.
    #[must_use]
    pub fn with_profile_permissions(mut self, permissions: Vec<PermissionContribution>) -> Self {
        self.profile_permissions = permissions;
        self
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
            profile_permissions: Vec::new(),
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
    let placements: Vec<Placement> = plan
        .mounts
        .iter()
        .map(|contribution| contribution.placement.clone())
        .collect();
    let contributed_targets: Vec<String> = placements
        .iter()
        .map(|placement| canonicalize_container_target(&placement.container_target))
        .collect();
    let baselines: Vec<String> = FULL_BASELINE_ROUTES
        .iter()
        .map(|route| route.to_string())
        .collect();
    let mut assembled_permissions = context.profile_permissions.clone();
    assembled_permissions.extend(plan.permissions.iter().cloned());
    let effective = EffectivePermissions::assemble(
        &context.occupied_mounts,
        &placements,
        &assembled_permissions,
        &baselines,
    )?;
    // Validation view: occupied driver triples plus contributed
    // placements rendered with their joined rights (Read→Ro,
    // else Rw). The identical-source RO+RO admission keys off
    // declared rights exactly as before; coverage was proven in
    // `assemble`, so every lookup hits.
    let mut triples: Vec<MountTriple> = context.occupied_mounts.clone();
    for placement in &placements {
        let target = canonicalize_container_target(&placement.container_target);
        let grant = effective
            .get(&target)
            .expect("assemble proves placement coverage");
        triples.push(MountTriple {
            host_source: placement.host_source.clone(),
            container_target: placement.container_target.clone(),
            mode: if grant.rights == Rights::Read {
                MountMode::Ro
            } else {
                MountMode::Rw
            },
        });
    }
    validate_mounts(&triples, &context.container_home).map_err(contract_error)?;
    let policy_claims = check_claims(&plan.policy_claims)?;
    let guest_hooks = order_hooks(&plan.guest_hooks)?;
    Ok(MergedPlan {
        environment,
        placements,
        permissions: effective.select_sorted(&contributed_targets),
        policy_claims,
        guest_hooks,
        cleanup: plan.cleanup,
    })
}

fn contract_error(error: CistellaError) -> CistellaError {
    CistellaError::Contract(error.to_string())
}

pub(crate) fn gate_capabilities(plan: &PreparePlan, advertised: &CapabilitySet) -> Result<()> {
    for (nonempty, capability, what) in [
        (
            !plan.environment.is_empty(),
            Capability::Environment,
            "environment",
        ),
        (!plan.mounts.is_empty(), Capability::Mounts, "mounts"),
        (
            !plan.permissions.is_empty(),
            Capability::Permissions,
            "permissions",
        ),
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
        (!plan.cleanup.is_empty(), Capability::Cleanup, "cleanup"),
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
    for (index, contribution) in contributions.iter().enumerate() {
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
            // Name every claimant in plan order (first plus this
            // plus any later same-name contributions): a collision
            // refusal that names only two of three still hides an
            // author.
            let mut claimants: Vec<String> = contributions
                .iter()
                .take(index + 1)
                .filter(|other| other.name == contribution.name)
                .map(|other| format!("{:?}", other.provenance))
                .collect();
            for other in contributions.iter().skip(index + 1) {
                if other.name == contribution.name
                    && !claimants.contains(&format!("{:?}", other.provenance))
                {
                    claimants.push(format!("{:?}", other.provenance));
                }
            }
            return Err(CistellaError::Contract(format!(
                "duplicate environment contribution {} from {}",
                contribution.name,
                claimants.join(", ")
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
        if hook.artifact.kind != HOOK_ARTIFACT_KIND_BLOB {
            return Err(CistellaError::Contract(format!(
                "guest hook artifact kind must be {HOOK_ARTIFACT_KIND_BLOB}: order {}",
                hook.order
            )));
        }
        if !is_sha256_hex(&hook.artifact.sha256) {
            return Err(CistellaError::Contract(format!(
                "guest hook artifact sha256 must be 64 lowercase hex: order {}",
                hook.order
            )));
        }
        if hook.artifact.source.registry.is_empty() {
            return Err(CistellaError::Contract(format!(
                "guest hook artifact registry must not be empty: order {}",
                hook.order
            )));
        }
        if hook.artifact.source.path.is_empty() {
            return Err(CistellaError::Contract(format!(
                "guest hook artifact path must not be empty: order {}",
                hook.order
            )));
        }
        if hook.staging != HOOK_STAGING_ISOLATOR {
            return Err(CistellaError::Contract(format!(
                "guest hook staging must be {HOOK_STAGING_ISOLATOR}: order {}",
                hook.order
            )));
        }
        if hook.argv_prefix.is_empty() {
            return Err(CistellaError::Contract(format!(
                "guest hook argv prefix must not be empty: order {}",
                hook.order
            )));
        }
        if hook.argv_prefix.len() > MAX_HOOK_ARGV {
            return Err(CistellaError::Contract(format!(
                "guest hook argv prefix exceeds composition bound: order {}",
                hook.order
            )));
        }
        if !hook.argv_prefix[0].starts_with('/') {
            return Err(CistellaError::Contract(format!(
                "guest hook argv prefix must name the absolute staged wrapper path: order {}",
                hook.order
            )));
        }
        if hook.probe.op.is_empty() {
            return Err(CistellaError::Contract(format!(
                "guest hook probe op must not be empty: order {}",
                hook.order
            )));
        }
        if hook.probe.timeout_ms == 0 {
            return Err(CistellaError::Contract(format!(
                "guest hook probe timeout must be nonzero: order {}",
                hook.order
            )));
        }
        if hook.on_failure != HOOK_ON_FAILURE_PRE_EXEC {
            return Err(CistellaError::Contract(format!(
                "guest hook on_failure must be {HOOK_ON_FAILURE_PRE_EXEC}: order {}",
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

/// Checks 64 lowercase hex (SHA-256 digest shape).
fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Isolator lifecycle state.
///
/// `state` returns only this enum; `inspect` returns the rich
/// read-only snapshot. Every transition stays a separate operation.
///
/// Lowercase wire spelling (`created|initiated|...`) matches the
/// isolator-contract schema on both `state` and `inspect` paths —
/// one form, never Debug-derived.
pub use crate::framework::lifecycle::{
    Assumption, BaselineBinding, CancelFlag, ExecutionHandle, ExtensionIdentity, LifecycleState,
    ReconciliationKey, UnitHandle,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// Parses a capability advertisement name.
///
/// Unknown names are ignored (forward-compatible strictness lives in
/// the merge gate: undeclared contribution TYPES refuse, unknown
/// advertisement STRINGS do not).
#[must_use]
pub fn parse_capability(name: &str) -> Option<Capability> {
    match name {
        "environment" => Some(Capability::Environment),
        "mounts" => Some(Capability::Mounts),
        "permissions" => Some(Capability::Permissions),
        "policy-claims" => Some(Capability::PolicyClaims),
        "guest-hooks" => Some(Capability::GuestHooks),
        "cleanup" => Some(Capability::Cleanup),
        "credentials" => Some(Capability::Credentials),
        _ => None,
    }
}

/// Merges one (or several concatenated) raw plans centrally:
/// gates contribution types against the negotiated capabilities,
/// merges atomically against the framework-owned baseline context
/// (destination collisions refuse), partitions claims, and
/// evaluates every contributed name against the lattice. Pair
/// composition concatenates declaration-ordered raw plans and
/// merges once here, so cross-plan collisions refuse
/// deterministically under the same validators.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on admission, merge, claim,
/// or policy refusal.
pub(crate) fn merge_transacted(
    plan: PreparePlan,
    credentials: Vec<AdmittedCredential>,
    guest_id: &str,
    capabilities: &[String],
    context: &MergeContext,
    policy: &PolicySet,
    acceptances: &HashSet<String>,
) -> Result<EvaluatedPlan> {
    let fallback = Provenance::Extension(guest_id.to_string());
    merge_transacted_as(
        plan,
        credentials,
        &|_| fallback.clone(),
        capabilities,
        context,
        policy,
        acceptances,
    )
}

/// Merges with per-name provenance: each contributed name
/// evaluates under the provenance of the plan that contributed
/// it (pair composition keeps per-source authorization instead
/// of collapsing to the first guest). Names missing from the
/// map fall back to `fallback` (unreachable when the map covers
/// every raw plan — merge invents no names).
///
/// # Errors
///
/// Returns `CistellaError::Contract` on admission, merge, claim,
/// or policy refusal.
pub(crate) fn merge_transacted_as(
    plan: PreparePlan,
    credentials: Vec<AdmittedCredential>,
    provenance_of: &dyn Fn(&str) -> Provenance,
    capabilities: &[String],
    context: &MergeContext,
    policy: &PolicySet,
    acceptances: &HashSet<String>,
) -> Result<EvaluatedPlan> {
    let advertised = CapabilitySet::new(
        &capabilities
            .iter()
            .filter_map(|name| parse_capability(name))
            .collect::<Vec<_>>(),
    );
    if !credentials.is_empty() && !advertised.allows(Capability::Credentials) {
        return Err(CistellaError::Contract(
            "guest returned unadvertised contribution type: credentials".to_string(),
        ));
    }
    let merged = merge_prepare(plan, &advertised, context)?;
    let contributed: Vec<String> = merged
        .environment
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let (claims, diagnostics) = policy.partition_claims(&merged.policy_claims, &contributed)?;
    for (name, _) in &merged.environment {
        policy.evaluate(name, &provenance_of(name), acceptances, &claims)?;
    }
    // Full effective map (driver feed plus joined contributions):
    // merge_prepare already proved the identical checks on the
    // same inputs, so this deterministic rebuild cannot refuse
    // anything the merge admitted. Unreachable refusal would be
    // an internal inconsistency, never operator input.
    let baselines: Vec<String> = FULL_BASELINE_ROUTES
        .iter()
        .map(|route| route.to_string())
        .collect();
    // Rebuild from the merged outcome: contributed placements plus
    // claimant entries re-enter as raw permissions (join is
    // commutative, associative, and idempotent, so claimant order
    // cannot change the result; the first pass already excluded
    // exact duplicates).
    let mut raw_permissions = Vec::new();
    for grant in &merged.permissions {
        for (provenance, rights) in &grant.claimants {
            raw_permissions.push(PermissionContribution {
                target: grant.target.clone(),
                rights: *rights,
                provenance: provenance.clone(),
            });
        }
    }
    let effective = EffectivePermissions::assemble(
        &context.occupied_mounts,
        &merged.placements,
        &raw_permissions,
        &baselines,
    )
    .expect("merge_prepare proves identical assembly checks");
    Ok(EvaluatedPlan {
        merged,
        effective,
        diagnostics,
        credentials,
    })
}
