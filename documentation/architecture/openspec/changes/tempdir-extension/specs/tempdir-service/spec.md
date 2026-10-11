# tempdir-service Specification

## Purpose
Per-session writable `/tmp` provided by a third-party extension: host-slice mint, RW triple contribution, teardown reap.
## ADDED Requirements
### Requirement: Per-session host slice lifecycle

A registered tempdir extension SHALL mint one host directory per session prepare (fixed prefix plus validated session id plus randomness, O_EXCL create-fail retry, owner-only mode), contribute it as a read-write triple targeting the container `/tmp`, and declare it in a `cleanup` list naming the minted directory. The framework SHALL remove ONLY declared cleanup paths after converged teardown (best-effort removal is litter, never residue — the unit is gone either way; undeclared contributed mounts are never reaped). Reap SHALL be nofollow recursive over directories only (symlinks and non-directories refuse, never followed or removed). Slice-name collision SHALL fail the prepare loudly, never share a slice between sessions.

#### Scenario: Session conducts with a writable host-backed /tmp
- **WHEN** a profile declares the tempdir extension and conducts
- **THEN** the harness observes a writable `/tmp` backed by a per-session host directory, and the declared directory is gone after teardown

#### Scenario: Slices never share between sessions
- **WHEN** two sessions conduct with the tempdir extension
- **THEN** each observes a distinct host directory; no bytes cross between slices

#### Scenario: Name collision fails loudly
- **WHEN** the minted slice name already exists as a non-directory or unusable object
- **THEN** prepare refuses with a typed error before any unit exists; no session shares the stranger

#### Scenario: Undeclared contributions survive teardown
- **WHEN** a session conducted with an extension that contributed mounts but declared no cleanup paths ends
- **THEN** the contributed host sources still exist after teardown (only declarations are deletion authority)

#### Scenario: Reap refuses symlinks
- **WHEN** a declared cleanup path names a symlink or non-directory at reap time
- **THEN** reap refuses that entry without following it; the refusal reports litter without failing the teardown verdict

#### Scenario: Cleanup capability advertised
- **WHEN** the tempdir extension advertises `mounts` and `cleanup` and contributes a cleanup list naming its minted directory
- **THEN** the merge gate admits it; a `cleanup` contribution with the type unadvertised refuses the whole transaction typed

### Requirement: Orphaned slice reconciliation

Slice names SHALL embed the session id as the durable record. If prepare mints then fails before responding (or the response is lost/invalid, leaving no merged plan to reap from), and for any slice whose session has no live unit, `gc` SHALL own retryable cleanup (never deleting under an active session). Crash and lost-response handling SHALL be specified paths, not assumptions.

#### Scenario: Lost prepare response still reconciles
- **WHEN** a slice exists for a session with no live unit and no merged plan referencing it
- **THEN** `gc` removes it; slices under active sessions are never touched

### Requirement: Reference extension vending

The reference tempdir extension SHALL build beside the framework tests (example-grade binary, never an installed sibling) and resolve only through the seat registry by absolute path — placement confers no discovery. Its hello role SHALL be its own registered name; it SHALL advertise `mounts` (the merge gate admits nonempty mounts only when advertised). Dogfood profiles SHALL name it by registry entry; fleet installs third-party tempdirs like any third-party guest. Promotion to a packaged artifact SHALL wait for the second third-party guest or fleet install need.

#### Scenario: Registry-resolved tempdir guest hosts
- **WHEN** the registry maps the tempdir name to a verified binary and a profile declares it
- **THEN** conduct hosts it exactly as any registered extension (prepare, merge, lifecycle); verification failures refuse pre-create typed
