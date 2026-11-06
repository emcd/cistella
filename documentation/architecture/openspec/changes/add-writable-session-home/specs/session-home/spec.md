## ADDED Requirements

### Requirement: Home source declaration

The profile schema SHALL accept optional `home-source-class` with value `container` (container-private tmpfs home) or `host` (host directory bind), plus `home-host-source` path required exactly when class is `host`. An absent field SHALL mean container-private, the only topology the driver constructs today. Validation SHALL construct one authoritative home descriptor (canonical target + class + backing) bound into the compile snapshot fingerprint; the supervisor FULL derivation, the RO-retention sets, and the mount renderer SHALL all consume that descriptor, and a rendering computed from any other snapshot SHALL refuse at application. Unknown class values are typed refusals.

#### Scenario: Absent field on current topology

- **WHEN** a profile omits `home-source-class`
- **THEN** the home resolves as container-private with no error and the descriptor records backing tmpfs

#### Scenario: Host declaration requires a path

- **WHEN** a profile declares `home-source-class = "host"` without a host path
- **THEN** profile validation fails with a typed error naming the missing path

#### Scenario: Host source with absent or container class refuses

- **WHEN** a host home source is supplied while the class is absent or `container`
- **THEN** conduct fails with a typed contract error before any mutation

#### Scenario: Host with path refuses as unsupported this release

- **WHEN** a profile declares `home-source-class = "host"` with a host path
- **THEN** conduct fails with an explicit unsupported-construction refusal before unit or scratch creation

#### Scenario: Descriptor mutation invalidates the snapshot

- **WHEN** the home target, class, or backing in a snapshot is altered after fingerprinting
- **THEN** fingerprint recomputation disagrees and the plan refuses at application (no stale-plan reuse)

### Requirement: Admission authority and render derivation

The construction-capable home descriptor SHALL be minted by the driver's admitted construction validator as a closed driver-owned type; compile-only fixtures SHALL NOT constitute launch authority. Final create and render SHALL derive every home input (mount target and backing, unit tmpfs/volume rendering, HOME value, supervisor rule) exclusively from the admitted descriptor or verify equality to it before construction; no independently supplied home source, container-home spelling, raw volume list, or reconstructed profile value SHALL substitute a different target or backing after admission. Final topology validation SHALL still reject mounts at HOME or an ancestor shadowing the private root. This closes substitution within driver-owned inputs only; it claims no protection against a malicious container engine or in-process code.

#### Scenario: Freshly rehashed invalid descriptor refuses

- **WHEN** a snapshot carries class `container` paired with a host backing (or any invalid class/backing combination), freshly hashed
- **THEN** admission refuses with a typed error before any construction, staging, or guest preparation side effect

#### Scenario: Compile-only host handed to create refuses

- **WHEN** a compile-only host-class fixture reaches the create path
- **THEN** create refuses as unsupported construction before unit or scratch creation

#### Scenario: Render backing differing from admitted descriptor refuses

- **WHEN** the rendered home mount backing or target differs from the admitted descriptor (e.g. unit Tmpfs path vs admitted target)
- **THEN** construction refuses before the container is created

#### Scenario: Shadowing mount at HOME refuses

- **WHEN** a profile, CLI, or extension mount lands at HOME or an ancestor of the private root
- **THEN** validation refuses typed while admitted descendants and alias safeguards are retained

### Requirement: Writable container-private home

A container-private home target SHALL receive FULL Landlock rights and SHALL be folded into the FULL-route accounting, so the existing RO-retention machinery keeps read-only bindings on RO mounts nested beneath it.

#### Scenario: Write to unmounted home path

- **WHEN** a session with a container-private home writes outside every mount (e.g. a new dotfile)
- **THEN** the write succeeds

#### Scenario: Read-only mount stays read-only under a writable home

- **WHEN** a session with a container-private home writes through a nested RO mount
- **THEN** the write is refused at the VFS binding with the mount mode intact

#### Scenario: RW graft under RO parent under FULL home refuses with remedy

- **WHEN** a profile nests a read-write triple beneath a read-only ancestor under the newly FULL home
- **THEN** conduct refuses via the existing retained-RO contradiction preflight, and the remedy is to move the graft out from under the RO dir

#### Scenario: Host-class home is compile-only this release

- **WHEN** a future host-class home reaches the compiler (compile-only fixtures; no constructed session this release)
- **THEN** the home target receives R+X only, and any constructed-session attempt refuses per the unsupported-construction scenario above
