# framework-lifecycle Specification

## Purpose
Framework lifecycle (0.2.0): synced from earlier changes. Update Purpose after archive.
## MODIFIED Requirements
### Requirement: Hook slots with typed contributions

The framework SHALL expose hook slots with fixed schemas: isolator lifecycle calls, one extension `prepare` transaction returning separately-typed environment and mount contribution sets plus policy claims, guest-hook requests, and cleanup declarations, policy evaluation, and teardown observation. Capability advertisement declares which contribution types each guest may return; undeclared types are refused. Central merge applies existing per-type validation (mount topology, env gates, collision rules) in spine order. Teardown SHALL reap extension-declared cleanup paths after converged teardown (identity-recorded at admission, root-admitted, FD-traversed; removal failure is litter, never residue — reap runs only once termination and removal have converged, never while a surviving container or harness could use the paths). `gc` SHALL own retryable cleanup of attributable orphaned slices (session id embedded in names; never under an active session, a live driver, a pending spawn marker, any live recorded guest process group, or an unreadable lease; dead-driver/all-recorded-groups-dead leases are stale) with a durable record and crash/lost-response handling. `gc` SHALL enumerate only the session-attributable prefix pattern, never arbitrary declared names. This spec owns the policy interface (severity × scope, lattice semantics, tighten-only doctrine); claim expression over stdio is owned by the extension-protocol spec.

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
