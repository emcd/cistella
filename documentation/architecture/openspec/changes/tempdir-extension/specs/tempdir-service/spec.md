# tempdir-service Specification

## Purpose
Per-session writable `/tmp` provided by a third-party extension: host-slice mint, RW triple contribution, teardown reap.
## ADDED Requirements
### Requirement: Per-session host slice lifecycle

A registered tempdir extension SHALL mint one host directory per session prepare (fixed prefix plus validated session id plus randomness, O_EXCL create-fail retry, owner-only mode), contribute it as a read-write triple targeting the container `/tmp`, and declare it in a `cleanup` list naming the minted directory. The framework SHALL remove ONLY declared cleanup paths after converged teardown (best-effort removal is litter, never residue — the unit is gone either way; undeclared contributed mounts are never reaped). At merge/admission the framework SHALL record each declared root identity (dev+ino of the root plus its parent association). Each declared root SHALL be admitted at reap time (still names the recorded identity — a regular-directory replacement refuses exactly like a symlink swap — non-symlink, seat- or root-owned); traversal SHALL run via anchored directory FDs with every descended directory meeting traversal trust on its open description (seat-or-root owner, entry mutation restricted to trusted actors), ordinary files SHALL unlink without following, and nested symlinks SHALL unlink as entries without traversal. Slice-name collision SHALL fail the prepare loudly, never share a slice between sessions.

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

#### Scenario: Populated slice removes completely
- **WHEN** a declared cleanup root holds harness-created files, subdirectories, and nested symlinks (including a symlink pointing outside the slice)
- **THEN** reap removes the whole tree; the outside target survives untouched (links unlink as entries, never traversed)

#### Scenario: Swapped root refuses
- **WHEN** a declared cleanup path names a symlink, a non-directory, or a different directory (dev+ino mismatch against the recorded identity) at reap time
- **THEN** reap refuses that entry without following it; the refusal reports litter without failing the teardown verdict (directory-to-different-directory swaps pin without privileges)

#### Scenario: Cleanup capability advertised
- **WHEN** the tempdir extension advertises `mounts` and `cleanup` and contributes a cleanup list naming its minted directory
- **THEN** the merge gate admits it; a `cleanup` contribution with the type unadvertised refuses the whole transaction typed

### Requirement: Orphaned slice reconciliation

Slice names SHALL embed the session id as the durable record. The driver SHALL hold a prepare lease across each hosted prepare recording driver pid, a pending marker before each spawn (cleared by the matching record), plus one guest pid and process-group line per hosted guest, created O_EXCL before the first prepare spawn and atomically rewritten per guest, filed in the verified private per-seat state directory (removed once guest shutdown is proven and the unit lifecycle converges, or the failure converges with quiescence proven); `gc` SHALL skip sessions with a live driver, any live recorded guest process group, an unreadable/malformed lease, or a lease with no guest identity yet, SHALL treat dead-driver/all-recorded-groups-dead leases as stale, and SHALL never signal (observation is signal-0 only, so reused PIDs cannot be killed through the lease). A dead driver pid alone SHALL NOT imply guest quiescence. If prepare mints then fails before responding (or the response is lost/invalid, leaving no merged plan to reap from), and for any slice whose session has no live unit and no live lease, `gc` SHALL own retryable cleanup (never deleting under an active session or an active prepare). Crash and lost-response handling SHALL be specified paths, not assumptions. Unattended `gc` SHALL enumerate only the session-attributable prefix pattern — arbitrary declared names are reaped solely through explicit per-session plans, never by enumeration.

#### Scenario: Lost prepare response still reconciles
- **WHEN** a slice exists for a session with no live unit, no live lease, and no merged plan referencing it
- **THEN** `gc` removes it; slices under active sessions or live leases are never touched

#### Scenario: gc skips in-flight prepare
- **WHEN** `gc` runs while a prepare for the session is hosted (lease live, no unit yet)
- **THEN** the session slices survive; a dead-driver/dead-group (stale) lease is removed first and normal orphan rules apply after

#### Scenario: gc preserves uncertainty under surviving guests
- **WHEN** the driver is dead (SIGKILL) but the recorded guest process group is alive — e.g. the guest was paused pre-response
- **THEN** no reap runs until actual guest quiescence is proven (group dead by signal-0 poll); only then does orphan cleanup proceed

#### Scenario: Lease observations never kill
- **WHEN** lease pids have been reused by unrelated processes
- **THEN** `gc` still signals nothing; liveness reads may skip conservatively but never terminate a stranger

### Requirement: Reference extension vending

The reference tempdir extension SHALL build beside the framework tests (example-grade binary, never an installed sibling) and resolve only through the seat registry by absolute path — placement confers no discovery. Its hello role SHALL be its own registered name; it SHALL advertise `mounts` and `cleanup` (the merge gate admits nonempty contribution types only when advertised). Dogfood profiles SHALL name it by registry entry; fleet installs third-party tempdirs like any third-party guest. Promotion to a packaged artifact SHALL wait for the second third-party guest or fleet install need.

#### Scenario: Registry-resolved tempdir guest hosts
- **WHEN** the registry maps the tempdir name to a verified binary and a profile declares it
- **THEN** conduct hosts it exactly as any registered extension (prepare, merge, lifecycle); verification failures refuse pre-create typed
