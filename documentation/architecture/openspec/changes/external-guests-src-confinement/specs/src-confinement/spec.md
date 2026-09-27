## ADDED Requirements

### Requirement: Guest-visible topology translation

Landlock rules SHALL apply to guest-visible paths, not host pathnames: conduct SHALL translate the canonical host ancestor/subtree through the validated guest mount topology to every guest-visible route before restricting. Profiles with UNTRANSLATABLE aliases (symlinked ancestors with no guest route, undescribable shapes) SHALL refuse pre-create rather than confine partially. A second bind mount into the same sibling that DOES translate admits or denies BY DECLARED MODE: declared read-write grafts compose into full-rights carveouts (accounted, admitted), while second routes with no declared carveout stay denied. R+X ancestor plus full-rights subtree is correct union semantics only within one verified layer and a complete path view.

#### Scenario: Rules land on the guest target
- **WHEN** the guest mount topology maps the host ancestor to a different guest target path
- **THEN** the ruleset restricts the guest target (verified against the validated topology), not the host pathname; a keep-id seat where both coincide still translates rather than assuming identity

#### Scenario: Sibling write denied directly
- **WHEN** the harness attempts a write to a sibling subtree path
- **THEN** the write fails with `EACCES` and the session continues confined

#### Scenario: Accounted second route without carveout denied
- **WHEN** the harness reaches the same sibling through a second guest-visible bind recorded in the validated topology, where that bind carries NO declared read-write carveout
- **THEN** the write fails with `EACCES` on that guest path (a second bind WITH a declared read-write carveout admits by declared intent — see the guarantee-scope requirement)

#### Scenario: Unaccounted alias refuses pre-create
- **WHEN** a profile carries paths the topology cannot translate into guest routes (unresolvable host sources, undescribable shapes)
- **THEN** conduct refuses pre-create with a typed error; no session starts partially confined
- Declared mounts that DO translate grant by declared mode (operator direction: declarations are authoritative intent, including read-write carveouts under read-only trees); only the undescribable refuses

#### Scenario: Same-path pre/post restriction control
- **WHEN** the suite writes a marker to the guest-visible sibling path with the same uid/mount mode before restrictions apply, cleans the marker, applies Landlock, and repeats the identical write
- **THEN** the pre-restriction write succeeds and the post-restriction write fails with `EACCES`, attributing the denial to the ruleset and not a pre-existing kernel wall; repeat for every alternate-bind route

### Requirement: Subtree confinement from the session directory

The Landlock extension SHALL confine the worktree ancestor so that the session's project subtree is writable and every path under the ancestor WITHOUT a declared read-write carveout SHALL deny writes (declared read-write directory mounts grant full rights on their guest targets wherever they sit — per-project grafts, state dirs, scratch — by operator direction; declarations are authoritative intent). The ancestor/subtree source is conduct's already-canonicalized session directory (`~/src/<project>` or `~/src/CLONES/<project>/<lane>`); the ruleset itself SHALL grant read/execute on the translated guest ancestor route and full rights on the translated guest project-subtree route (Landlock union semantics make the exception exact). No new profile schema and no per-seat authored rules SHALL be required. Enforcement covers the wrapped harness lineage only (see the guarantee-scope requirement): companion shells are trusted and unconfined by operator decision.

#### Scenario: Project subtree writable
- **WHEN** the harness writes inside its project subtree
- **THEN** the write succeeds under the applied ruleset

#### Scenario: Sibling subtree without carveout denied
- **WHEN** the harness attempts a write elsewhere under the translated guest ancestor route, on a path with NO declared read-write carveout
- **THEN** the write fails with `EACCES` and the session continues confined (see the topology requirement for alternate routes; see the guarantee-scope requirement for declared carveouts)

## MODIFIED Requirements

### Requirement: Confinement guarantee scope (operator-revised 2026-09-26)

Modifies the subtree-confinement guarantee above under explicit operator direction (trusted operator, harness-only threat model). Enforcement SHALL cover the wrapped harness lineage ONLY: companion exec (`cistella enter`, plain `podman exec`) runs outside Landlock by design — the operator holds host-equivalent access, so restricting their own shell defends no principal. Declared read-write directory mounts SHALL each receive a full-rights Landlock carveout on their guest target wherever they sit (project grafts, state dirs, scratch); declared read-only mounts outside all routes SHALL receive read-execute readability; a declared read-only directory at or under a full-granted route SHALL keep its Podman binding read-only, and a read-only directory at or under FULL-backed content (the subtree, admitted read-write grafts) SHALL likewise retain read-only whatever its guest target (Landlock union cannot subtract the FULL grant through any alias — the VFS binding carries that enforcement), and a read-only directory holding a declared read-write descendant SHALL refuse pre-create as contradictory. A read-write graft of ancestor-tree content outside the subtree SHALL refuse pre-create (same-source different-rights binds alias at the dentry layer — Landlock is mount-agnostic — so the graft FULL would admit through every alias; move grafted content outside the tree or inside the subtree). Extension read-write mount contributions SHALL be refused (no Mounts policy admission exists); extension read-only contributions may merge (bus-socket vectors) but SHALL never compose into write grants.

#### Scenario: Declared graft admits writes
- **WHEN** the harness writes through a declared read-write graft of content from OUTSIDE the bound tree (disjoint dentries, e.g. `/opt/state` at a second guest target)
- **THEN** the write succeeds under the applied ruleset (same-tree grafts refuse pre-create instead — see the same-tree scenario: one source cannot carry two rights)

#### Scenario: Read-only alias under full grant denies
- **WHEN** the harness writes through a declared read-only directory nested under a full-granted route
- **THEN** the write fails (retained Podman read-only binding) and the source is unmodified

#### Scenario: Same-tree graft refuses pre-create
- **WHEN** a profile grafts ancestor-tree content read-write outside the subtree (same host source bound twice with different rights)
- **THEN** conduct refuses pre-create with a typed error; no session starts with an aliased grant

#### Scenario: Bind-mount alias refuses pre-create
- **WHEN** a profile grafts ancestor-tree content read-write through a bind-mount path outside the subtree (same dentry, different path — canonical spellings diverge, `st_dev`/`st_ino` or filesystem-relative containment agree)
- **THEN** conduct refuses pre-create with a typed error naming the graft target; no session starts with an aliased grant

#### Scenario: Subtree escape refuses pre-create
- **WHEN** a profile declares a read-write graft path-wise inside the subtree whose dentry sits outside it (a bind escape mounted under the subtree)
- **THEN** conduct refuses pre-create with a typed error naming the graft target; the carveout never grants undeclared content

#### Scenario: Companion shell outside the guarantee
- **WHEN** the operator enters a hooked session via companion exec
- **THEN** the shell runs unconfined (no Landlock rules apply); the session marker records confinement state only

#### Scenario: Extension read-write mount refused
- **WHEN** an extension prepare response carries a read-write mount contribution
- **THEN** conduct refuses the transaction pre-create with a typed error; no session starts

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
