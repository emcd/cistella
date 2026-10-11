# framework-lifecycle Specification

## Purpose
Framework lifecycle (0.2.0): synced from earlier changes. Update Purpose after archive.
## MODIFIED Requirements
### Requirement: Hook slots with typed contributions

The framework SHALL expose hook slots with fixed schemas: isolator lifecycle calls, one extension `prepare` transaction returning separately-typed environment and mount contribution sets plus policy claims and guest-hook requests, policy evaluation, and teardown observation. Capability advertisement declares which contribution types each guest may return; undeclared types are refused. Central merge applies existing per-type validation (mount topology, env gates, collision rules) in spine order. Teardown SHALL reap extension-contributed host state named in the merged plan (host directories contributed by extensions, removed after converged teardown; removal failure is litter, never residue). This spec owns the policy interface (severity × scope, lattice semantics, tighten-only doctrine); claim expression over stdio is owned by the extension-protocol spec.

#### Scenario: Undeclared contribution refused
- **WHEN** an extension returns a contribution type absent from its advertised capabilities
- **THEN** the framework refuses the whole prepare transaction with a typed error before any mutation

#### Scenario: Cross-contribution planning is atomic
- **WHEN** one prepare transaction returns both environment and mount sets
- **THEN** central validation sees both sets together and either accepts the merged plan or refuses it whole; partial application never occurs

#### Scenario: Contributed host state reaped
- **WHEN** a session conducted with an extension that contributed host directories ends (clean or failed teardown)
- **THEN** the contributed directories are removed after convergence; a removal failure reports litter without failing the teardown verdict
