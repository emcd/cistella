# tempdir-service Specification

## Purpose
Source confinement: synced from change tmpdir-promotion. Update Purpose after archive.

NOTE (rename at sync): this change renames the capability `tempdir-service` to
`tmpdir-service`. The delta below carries the full new requirement text; the sync
step moves `openspec/specs/tempdir-service/` to `openspec/specs/tmpdir-service/`
(retitle + role rename + production-grade vending) and deletes the old directory.
## MODIFIED Requirements
### Requirement: Per-session host slice lifecycle

A registered tmpdir extension SHALL mint one host directory per session prepare (fixed prefix plus validated session id plus randomness, O_EXCL create-fail retry, owner-only mode), contribute it as a read-write triple targeting the container `/tmp`, and declare it in a `cleanup` list naming the minted directory. The framework SHALL remove ONLY declared cleanup paths after converged teardown (best-effort removal is litter, never residue — the unit is gone either way; undeclared contributed mounts are never reaped). At merge/admission the framework SHALL record each declared root identity (dev+ino of the root plus its parent association). Each declared root SHALL be admitted at reap time (still names the recorded identity — a regular-directory replacement refuses exactly like a symlink swap — non-symlink, seat- or root-owned); traversal SHALL run via anchored directory FDs with every descended directory meeting traversal trust on its open description (seat-or-root owner; no group/other write bits, or sticky with a trusted owner — stricter than the shared mount-ancestor rule on purpose, since deletion unlinks by name and any rename-capable actor could swap the final target), ordinary files SHALL unlink without following, and nested symlinks SHALL unlink as entries without traversal. Slice-name collision SHALL fail the prepare loudly, never share a slice between sessions.

#### Scenario: Session conducts with a writable host-backed /tmp
- **WHEN** a profile declares the tmpdir extension and conducts
- **THEN** the harness observes a writable `/tmp` backed by a per-session host directory, and the declared directory is gone after teardown

#### Scenario: Slices never share between sessions
- **WHEN** two sessions conduct with the tmpdir extension
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
- **WHEN** the tmpdir extension advertises `mounts` and `cleanup` and contributes a cleanup list naming its minted directory
- **THEN** the merge gate admits it; a `cleanup` contribution with the type unadvertised refuses the whole transaction typed

### Requirement: Shipped extension vending

The tmpdir extension SHALL ship as the `cistella-extension-tmpdir` sibling binary (installed and discovered beside the driver, like the Landlock extension) and resolve through the seat registry by name with digest-pinned verification — placement confers no discovery. Its hello role SHALL be `tmpdir`; it SHALL advertise `mounts` and `cleanup` (the merge gate admits nonempty contribution types only when advertised). Profiles SHALL name it by registry entry; fleet installs it like any third-party guest.

#### Scenario: Registry-resolved tmpdir guest hosts
- **WHEN** the registry maps the tmpdir name to a verified binary and a profile declares it
- **THEN** conduct hosts it exactly as any registered extension (prepare, merge, lifecycle); verification failures refuse pre-create typed
