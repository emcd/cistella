# extension-hooks Specification

## Purpose
Hook registry: prepare, compile, inject-supervisor, and finalize phases with claimant rules and refusal shapes.
## Requirements

### Requirement: Hook registry with three phases

The framework SHALL execute the concrete phase sequence: resolve → lease → host in declaration order → per-guest prepare plus per-plan gate → collect → admission → concat → atomic central merge → lattice evaluation → compile → supervisor plan → stage/launch → attestation gate → converged teardown → finalize observations. Prepare runs BEFORE the merge (it supplies the merge inputs); lattice evaluation is complete before supervisor and finalize dispatch, never before prepare dispatch. The four hook phases are: `prepare` (many claimants; mounts/env/permissions IO after the split), `compile` (AT MOST ONE claimant; frozen admitted snapshot in, typed rendered plan out — see the compile requirement below), `inject-supervisor` (AT MOST ONE claimant; inputs are confinement roots plus merged placements plus the effective-permissions map plus declaration index — the map, not re-read modes, derives the grants), and `finalize` (many claimants; records already-admitted declarations and outcomes only — creates no actions and no new deletion authority). Artifact and executable admission, the selected-but-empty refusal, and ABI/attestation gating stay central even though no extension holds merge privilege.

#### Scenario: Prepare supplies the merge
- **WHEN** three extensions each return hook-phase prepare contributions with valid shape
- **THEN** all three dispatch in declaration order before the merge and their contributions enter it; nothing dispatches post-merge except compile, supervisor planning, and finalize observation

#### Scenario: Double claimant refuses naming both
- **WHEN** two extensions each contribute a nonempty guest-hook set
- **THEN** admission refuses pre-merge with a typed error naming both claimants; no session exists yet

#### Scenario: One guest returning two hooks refuses
- **WHEN** a single extension returns two hook requests in its set
- **THEN** admission refuses pre-merge exactly as for two claimants; counting nonempty plans alone never admits a second hook

#### Scenario: Single supervisor composes unchanged
- **WHEN** exactly one extension contributes exactly one hook request
- **THEN** the framework composes the wrapper argv from confinement roots, merged placements, the effective-permissions map, and declaration index exactly as today

#### Scenario: Finalize records without acting
- **WHEN** extensions declare finalize observations after a converged session
- **THEN** the framework records already-admitted declarations and outcomes only; a finalize path requesting new actions, new paths, or a live guest refuses typed as out of scope

#### Scenario: Launch asserts the admission invariant
- **WHEN** a merged plan reaches launch with more than one hook set or more than one request (admission bug, never operator input)
- **THEN** launch-plan refuses pre-create as defense in depth, independent of the admission refusal

### Requirement: Post-prepare pre-isolation compile phase

The framework SHALL run a compile phase after lattice evaluation and before supervisor planning and isolation: AT MOST ONE extension claims it; the input is the frozen admitted snapshot (canonical placement IDs with source/target identities, normalized grants, original binding intent, provenance, backend capabilities, snapshot fingerprint); the output is a typed rendered plan (rendered binding modes for existing binding IDs plus the backend policy/supervisor artifact). The compiler MAY change rendered modes for existing binding IDs; it SHALL NOT edit admission intent, invent or change host sources/targets, touch env/credentials/cleanup, reorder ownership, or reopen decided lattice policy. Compiler output binds to the snapshot AND the selected enforcement consumer (a rendering paired with an unrelated supervisor policy refuses). Unknown, missing, or duplicate placement IDs, missing input bindings, stale fingerprints, and unsupported enforcement refuse before create. A legitimate isolator with no binding renderer SHALL declare placements unsupported/not-applicable rather than silently dropping requested placements. Bounded frame deadlines, lease tracking, and reverse shutdown cover a retained-guest compile phase; no live finalize is created by this phase.

#### Scenario: Compiler renders the materialization rewrite
- **WHEN** the landlock-owned compiler receives a snapshot with a declared-RO directory outside FULL grants
- **THEN** its rendered plan flips the binding read-write for materialization with the RO intent carried into permissions, source declarations stay frozen, and framework gates validate the candidate before create

#### Scenario: Stale fingerprint refuses
- **WHEN** a compiler returns a rendered plan bound to a superseded snapshot fingerprint
- **THEN** the framework refuses before create; rendering never applies to a snapshot it was not computed from

#### Scenario: Compiler overreach refuses
- **WHEN** a compiler candidate changes a host source, adds a cleanup path, or pairs its rendering with an unselected supervisor policy
- **THEN** the framework refuses before create with a typed error naming the overreach domain; authority metadata is unchanged by compilation

#### Scenario: Double compile claimant refuses
- **WHEN** two extensions each claim the compile phase
- **THEN** admission refuses pre-merge naming both claimants, exactly like double supervisor claimants
