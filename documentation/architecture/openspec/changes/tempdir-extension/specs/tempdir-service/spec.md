# tempdir-service Specification

## Purpose
Per-session writable `/tmp` provided by a third-party extension: host-slice mint, RW triple contribution, teardown reap.
## ADDED Requirements
### Requirement: Per-session host slice lifecycle

A registered tempdir extension SHALL mint one host directory per session prepare (uniquely named per invocation, session id embedded for attribution, owner-only mode), contribute it as a read-write triple targeting the container `/tmp`, and the framework SHALL remove the contributed directory after converged teardown (best-effort removal is litter, never residue — the unit is gone either way). Slice-name collision SHALL fail the prepare loudly, never share a slice between sessions.

#### Scenario: Session conducts with a writable host-backed /tmp
- **WHEN** a profile declares the tempdir extension and conducts
- **THEN** the harness observes a writable `/tmp` backed by a per-session host directory, and the directory is gone after teardown

#### Scenario: Slices never share between sessions
- **WHEN** two sessions conduct with the tempdir extension
- **THEN** each observes a distinct host directory; no bytes cross between slices

#### Scenario: Name collision fails loudly
- **WHEN** the minted slice name already exists as a non-directory or unusable object
- **THEN** prepare refuses with a typed error before any unit exists; no session shares the stranger

### Requirement: Reference extension vending

The reference tempdir extension SHALL build as its own artifact (own binary target, own release packaging) and resolve only through the seat registry — never as a shipped sibling. Its hello role SHALL be its own registered name; it SHALL advertise `mounts` (the merge gate admits nonempty mounts only when advertised). Dogfood profiles SHALL name it by registry entry; fleet installs it like any third-party guest.

#### Scenario: Registry-resolved tempdir guest hosts
- **WHEN** the registry maps the tempdir name to a verified binary and a profile declares it
- **THEN** conduct hosts it exactly as any registered extension (prepare, merge, lifecycle); verification failures refuse pre-create typed
