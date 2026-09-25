## ADDED Requirements

### Requirement: Guest-visible topology translation

Landlock rules SHALL apply to guest-visible paths, not host pathnames: conduct SHALL translate the canonical host ancestor/subtree through the validated guest mount topology to every guest-visible route before restricting. Profiles with unaccounted aliases (symlinked ancestors, second bind mounts into the same sibling) SHALL refuse pre-create rather than confine partially. R+X ancestor plus full-rights subtree is correct union semantics only within one verified layer and a complete path view.

#### Scenario: Rules land on the guest target
- **WHEN** the guest mount topology maps the host ancestor to a different guest target path
- **THEN** the ruleset restricts the guest target (verified against the validated topology), not the host pathname; a keep-id seat where both coincide still translates rather than assuming identity

#### Scenario: Sibling write denied directly
- **WHEN** the harness attempts a write to a sibling subtree path
- **THEN** the write fails with `EACCES` and the session continues confined

#### Scenario: Accounted second route denied
- **WHEN** the harness reaches the same sibling through a second guest-visible bind recorded in the validated topology
- **THEN** the write fails with `EACCES` on that guest path

#### Scenario: Unaccounted alias refuses pre-create
- **WHEN** a profile carries a symlinked ancestor or second bind mount into a sibling that the topology validation does not account for
- **THEN** conduct refuses pre-create with a typed error; no session starts partially confined

#### Scenario: Same-path pre/post restriction control
- **WHEN** the suite writes a marker to the guest-visible sibling path with the same uid/mount mode before restrictions apply, cleans the marker, applies Landlock, and repeats the identical write
- **THEN** the pre-restriction write succeeds and the post-restriction write fails with `EACCES`, attributing the denial to the ruleset and not a pre-existing kernel wall; repeat for every alternate-bind route

### Requirement: Subtree confinement from the session directory

The Landlock extension SHALL confine the worktree ancestor so that only the session's project subtree is writable; everything else under the ancestor SHALL deny writes. The ancestor/subtree source is conduct's already-canonicalized session directory (`~/src/<project>` or `~/src/CLONES/<project>/<lane>`); the ruleset itself SHALL grant read/execute on the translated guest ancestor route and full rights on the translated guest project-subtree route (Landlock union semantics make the exception exact). No new profile schema and no per-seat authored rules SHALL be required.

#### Scenario: Project subtree writable
- **WHEN** the harness writes inside its project subtree
- **THEN** the write succeeds under the applied ruleset

#### Scenario: Sibling subtree denied
- **WHEN** the harness attempts a write elsewhere under the translated guest ancestor route
- **THEN** the write fails with `EACCES` and the session continues confined (see the topology requirement for alternate routes)

### Requirement: Complete write-denial rights coverage

The ruleset SHALL cover every write-capable Landlock right at the supported ABI, including `LANDLOCK_ACCESS_FS_TRUNCATE` (ABI v3: `open(O_RDONLY|O_TRUNC)` truncates despite WRITE_FILE denial). Conduct SHALL require the kernel ABI/handled-rights minimum sufficient for the full matrix and fail pre-exec with a typed capability error when unavailable. The denial matrix SHALL pin write, create, unlink/rename, and truncate cases — a single `EACCES` from one attempted write is not a complete proof.

#### Scenario: Truncate denied
- **WHEN** the harness opens a sibling file `O_RDONLY|O_TRUNC`
- **THEN** the open fails with `EACCES` and the file is unmodified

#### Scenario: Open-for-write denied
- **WHEN** the harness opens a sibling file for writing (`WRITE_FILE`)
- **THEN** the open fails with `EACCES`

#### Scenario: Create, unlink, and rename denied
- **WHEN** the harness creates a file in, unlinks a file in, or renames into a sibling subtree
- **THEN** each operation fails with `EACCES`

#### Scenario: Insufficient ABI fails pre-exec
- **WHEN** the kernel cannot enforce the full rights matrix
- **THEN** apply never runs and conduct fails pre-execute; no harness starts partially confined

### Requirement: Denial evidence gate

Confinement SHALL be proven by denial evidence from attempted writes in live conformance (admitted write succeeds, sibling write fails `EACCES`, unsupported kernels yield typed `Unsupported` pre-execute), never by design review alone. The unchanged-seat proof and fleet sweep SHALL repeat against the external path.

#### Scenario: Evidence over assertion
- **WHEN** the confinement suite runs on a supporting kernel
- **THEN** all three outcomes (admit/deny/unsupported-shape) are observed from real attempts, recorded per case

### Requirement: Fail-closed confinement

Confinement SHALL be fail-closed or absent-loudly, never absent-silently: where Landlock cannot apply, conduct fails pre-execute with a typed capability error. A session SHALL never run unconfined while believing itself confined.

#### Scenario: Unsupported kernel refuses loudly
- **WHEN** the guest-context capability probe reports Landlock unsupported
- **THEN** apply never runs and conduct fails pre-execute; no harness starts outside the ruleset
