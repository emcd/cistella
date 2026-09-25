## Context

0.1.x concentrates all behavior in the driver: `conduct` resolves, validates, prepares, starts, execs, and tears down, with Podman specifics inline (`src/transport.rs`, `src/runtime.rs`, `src/mount.rs` preparation) and caller wiring hand-rolled per profile. The standup converged the full constraint set (authority split ratified with site > user > per-profile > defaults, tighten-only; exact-names doctrine; no-deny precedent; Latinate contract verbs; pre-1.0 breakage acceptable). QA owns conformance + Landlock evidence; Owner owns interface/spec + Podman extraction.

## Goals / Non-Goals

**Goals:** framework-owned lifecycle with typed, centrally-merged contributions; Podman behind a contract it did not define; one real extension proving the protocol; policy as interface input; conformance that outlives any single backend.

**Non-Goals:** additional isolators; Agentmux/SSH migrations; mid-session observation; per-profile denials; QoL riders; work-time estimates (team works in hours; constraint is not-weeks/months).

## Decisions

- **Contract before trait.** Write the lifecycle/capability contract (phases, contribution schemas, merge rules) first; extract the trait against it with conformance binding from the first commit. Rejected: refactoring Podman call sites into a trait directly (lets accidents define the abstraction).
- **One prepare transaction, typed sets.** A single exchange returns `{environment: [...], mounts: [...], policy_claims: [...], guest_hooks: [...]}`; framework validates each set with its existing rules and merges centrally in spine order. Rejected: separate env/mount processes (loses atomic cross-contribution planning, multiplies helper launches) and one untyped schema (erases validation). Split executables/authorities remain possible — the transaction is the unit, not the process.
- **Host preparers vs guest pre-exec, explicit.** Host helpers run pre-start and return plans; guest components (Landlock wrapper) become the exec ancestor with a dedicated diagnostics FD. The Landlock spike must prove placement, ancestry, and Podman/user-namespace rule preservation before the protocol freezes.
- **Synchronous spine, bounded everything.** Framework sequencing is deterministic; guests may be async internally but every request has a bounded response. Framework owns deadlines: short hello, bounded plan/apply, SIGTERM grace then SIGKILL, capped frames/messages, typed timeouts, fail-closed pre-exec, reverse cleanup with residue-dominated reporting. No long-lived observers.
- **Length-prefixed stdio framing.** NDJSON demands stdout purity that untrusted guests cannot promise; length prefix + max frame + version negotiation + deterministic refusal of unknown fields/versions. Extension stdout is protocol-only; logs go to stderr; executables are discovered/pinned, never PATH-searched.
- **Policy lattice enforced at two gates.** Pre-merge evaluation (contribution admissibility) and pre-exec enforcement; profiles declare capabilities, exact acknowledgements live in user `policies.toml` scope, never overrides; site floor is absolute. Suppressible violations refuse unless acknowledged; diagnostics stay value-free.
- **Credential handles, never values.** The seam schema has reference/handle fields only; secret-shaped content is structurally unrepresentable. Denial evidence comes from attempted syscalls in conformance, not live observers.
- **Fake from series one.** A deterministic test-double ships with the protocol for boundary validation (malformed/oversize/partial/timeout/version-refusal/cleanup-ordering); it must not drive lifecycle semantics — Podman does that.

## Risks / Trade-offs

- [Risk] Contract-first slows the first diff → Mitigation: vertical slice is small by design (one isolator, one extension); conformance binds immediately so the contract stays honest.
- [Risk] Single-transaction prepare becomes a bottleneck for slow guests → Mitigation: bounded responses with kill semantics; slowness surfaces as failure, not latency.
- [Risk] Podman extraction reveals behavior the contract didn't capture → Mitigation: that discovery IS the work; conformance cases get added, never silently absorbed.
- [Risk] Fleet seats need rewrites → Mitigation: dogfood-visibility condition is a release gate — 0.1.x profiles run unchanged or the slice isn't done.

## Migration Plan

Land as internal reorganization + additive protocol/policy modules behind the existing `conduct` surface; fleet seats untouched. No migration of existing profile/session state; the new user `policies.toml` format (schema, versioning, rollback handling) is in scope. Rollback is commit revert.

## Open Questions

- Trixie image chore timing (pre-release, PC Setup lane).
