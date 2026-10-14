# framework-lifecycle Specification

## Purpose
Framework lifecycle: synced from change extension-composition. Update Purpose after archive.
## MODIFIED Requirements
### Requirement: Hook slots with typed contributions

The framework SHALL expose hook slots with fixed schemas: isolator lifecycle calls, one extension `prepare` transaction per declared extension returning separately-typed environment, mount (placement-only), permission, and policy-claim contribution sets plus guest-hook requests and cleanup declarations, policy evaluation, and teardown observation. Capability advertisement declares which contribution types each guest may return; undeclared types are refused. Each raw plan SHALL gate against its own negotiated capabilities pre-concat — the gate covers the full transacted unit (plan plus credentials plus permissions), so no guest launders a contribution type through another guest's advertisement; policy claims are scoped guest-locally (a guest's claims may target only names it contributed in the same transaction; cross-guest targeting refuses typed). Plans concatenate in declaration order; the single central merge stays atomic under existing validators — collisions across plans refuse deterministically naming ALL claimants, same-target rights join deterministically (Read ∪ Write = Full) with every claimant recorded, duplicate extension declarations refuse at resolution, and lattice evaluation sees each contribution under its host-assigned per-name provenance (multimap — no last-writer attribution; a missing provenance entry is a typed internal refusal, never a guessed identity). At most one nonempty guest-hook set containing at most one hook request is admitted, naming all claimants on refusal (the wrapper chain is the only true ceiling). Intent authority: contributed placements derive bindings from intent; driver bindings render from declared modes and extension intent never vetoes them (recorded for audit and retention input). Post-merge the framework SHALL compute one effective-permissions map (canonical target → joined minimum grants with all provenances, PLUS separate enforcement-bearing binding intent: the most restrictive declared binding per target — read-only if ANY claimant declared read-only, including driver ro and file bindings) as the single source of truth for grant computation, alias guards, RO-retention, descendant checks, topology rendering, the preserved safe RO→RW revision (declared-RO-outside-FULL renders read-write with Landlock carrying the declared intent; retention where the union cannot subtract; files verbatim; revision runs only when a hook is applied — plain sessions render declared modes verbatim), the compile phase input snapshot, and supervisor argv composition. Framework validation of compiler output covers closed output grammar, input coverage and identity, snapshot binding, delegation/claimant limits, unchanged authority metadata, and no extra mutation domains — never recreated backend algorithms. Central merge applies existing per-type validation (mount topology, env gates, collision rules) in spine order. The final topology gate SHALL validate the merged set, run the nested-RO preflight, then pre-create nested bind mountpoints scoped to declared cleanup roots (anchored nofollow walk, 0700; other nesting keeps podman behavior). Teardown SHALL reap extension-declared cleanup paths after converged teardown (identity-recorded at admission, root-admitted, FD-traversed; removal failure is litter, never residue — reap runs only once termination and removal have converged, never while a surviving container or harness could use the paths; the reap covers orderly terminate and natural harness completion alike). `gc` SHALL own retryable cleanup of attributable orphaned slices (session id embedded in names; never under an active session, a live driver, a pending spawn marker, any live recorded guest process group, or an unreadable lease; dead-driver/all-recorded-groups-dead leases are stale) with a durable record and crash/lost-response handling. `gc` SHALL enumerate only the session-attributable prefix pattern, never arbitrary declared names. This spec owns the policy interface (severity × scope, lattice semantics, tighten-only doctrine); claim expression over stdio is owned by the extension-protocol spec. Hook phase dispatch (prepare many, inject-supervisor at most one, finalize many observing) is owned by the extension-hooks spec.

#### Scenario: Undeclared contribution refused
- **WHEN** an extension returns a contribution type absent from its advertised capabilities
- **THEN** the framework refuses the whole prepare transaction with a typed error before any mutation

#### Scenario: Cross-contribution planning is atomic
- **WHEN** one prepare transaction returns both environment and mount sets
- **THEN** central validation sees both sets together and either accepts the merged plan or refuses it whole; partial application never occurs

#### Scenario: Contributed host state reaped
- **WHEN** a session conducted with an extension that declared cleanup paths ends with converged teardown
- **THEN** the declared directories are removed; a removal failure reports litter without failing the teardown verdict; undeclared contributed mounts survive

#### Scenario: Reap waits for convergence
- **WHEN** termination or removal has not converged (a container or harness may survive)
- **THEN** no reap runs; the paths remain until convergence or `gc`

#### Scenario: gc owns orphaned slices
- **WHEN** slices exist for sessions with no live unit (including prepare-failure orphans with no merged plan)
- **THEN** `gc` removes them retryably; slices under active sessions are never touched

#### Scenario: Nested mountpoints pre-created under cleanup roots
- **WHEN** the merged topology nests a bind under another bind whose host source sits at or under a declared cleanup root
- **THEN** the framework pre-creates the mountpoint seat-owned before container start through an anchored nofollow walk (planted symlinks refuse); nesting elsewhere keeps podman behavior

#### Scenario: N plans merge in declaration order
- **WHEN** a profile declares three extensions that each answer prepare validly
- **THEN** each raw plan gates against its own negotiated capabilities, the plans concatenate in declaration order, and one atomic central merge admits them with per-name provenance

#### Scenario: Collision names all claimants
- **WHEN** two extensions contribute the same environment name with different values
- **THEN** the merge refuses with a typed error naming both claimants; no partial plan applies

#### Scenario: Same-target rights join with all provenances
- **WHEN** two extensions declare rights on the same canonical container target (`Read` and `Full`)
- **THEN** the effective permission joins to `Full` deterministically with both claimants recorded; no refusal, no silent widening beyond the join

#### Scenario: Credential laundering refuses pre-concat
- **WHEN** GuestA returns credentials it never advertised while GuestB advertises the credentials type
- **THEN** GuestA's own per-plan gate refuses before concatenation; the later union gate never sees the laundered unit

#### Scenario: Cross-guest claim targeting refuses
- **WHEN** GuestA's policy claim targets a name only GuestB contributed in the same session
- **THEN** GuestA's guest-local scope check refuses typed; claims never evaluate against another guest's names

#### Scenario: Missing provenance refuses internal
- **WHEN** a merged name carries no recorded contributor (merge bug, never operator input)
- **THEN** evaluation refuses with a typed internal error; no fallback identity is guessed
