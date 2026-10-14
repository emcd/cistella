# mounts Specification

## Purpose
Mounts: synced from change extension-composition. Update Purpose after archive.
## ADDED Requirements
### Requirement: Extension-contributed placement rendering

Extension-contributed placements SHALL arrive modeless (`host_source`, `container_target` only); their Podman binding modes SHALL render from the post-merge effective-permissions map through the preserved safe revision: effective `Read`-only directories outside FULL grants render read-write so submounts materialize with the Landlock policy (derived from the ORIGINAL declared intent) enforcing the read-only in-container; Podman read-only is retained exactly where the Landlock union cannot subtract (target at/under a FULL route, source at/under a FULL source); file sources always render their declared mode verbatim. `Write` SHALL normalize to effective `Full` at map construction before every consumer (retention, alias guards, rendering, revision included), with original `(claimant, label)` pairs preserved for audit but deciding nothing after normalization. Permission labels are minimum grants; original binding intent is separate enforcement-bearing state per target (most restrictive declared binding — read-only if ANY claimant declared read-only). Config surface: a `[[permissions]]` array (path + rights under the framework-defined closed vocabulary, framework-validated, guest-visible namespace, source-labelled entries) is the policy layer; permissions written on `[[mounts]]` entries are sugar decomposing once, early, into the canonical IR (ro→placement + `Read` grant + read-only intent; rw→placement + `Full` grant + writable intent; never emitted twice). Grants join across sugar and explicit entries while binding floors resolve most-restrictive; contradictions surface typed, never silent. Absent-defaults per class: legacy driver modes imply policy; extension placements without matching rights refuse; known baselines come from the owning provider; unmatched explicit targets fail typed. `[supplements]` knobs stay per-extension, guest-validated, and SHALL NOT carry permission overrides: a closed reserved key namespace marks permission-shaped keys, recognized generically without backend-specific inference — matching keys refuse typed, and the owning guest's declared knob schema rejects the rest. File/kernel binding constraints are represented explicitly in the IR even where config stays sugary. Driver-declared triples (profile, CLI, worktree, baseline) SHALL keep their declared intent untouched at the declaration layer and feed the map with driver provenance (ro→`Read`, rw→`Full`). The safe revision runs only when a hook is applied; plain sessions render declared modes verbatim. Rights grant Landlock permission only: they never widen a retained read-only VFS binding, in hooked and plain sessions alike — grant-only or omitted rights never silently widen a declared read-only binding.

#### Scenario: Contributed binding renders from rights
- **WHEN** an extension contributes a placement with a `Write` permission and the merge admits it
- **THEN** the container binding mounts read-write; the mode came from the permission, not the placement

#### Scenario: Declared RO outside FULL revises with Landlock carrying the intent
- **WHEN** a profile declares `~/src` read-only with a nested read-write project submount and no FULL grant covers the ancestor
- **THEN** the ancestor renders Podman read-write for materialization while the Landlock policy enforces the declared read-only, and project writes succeed through the nested submount exactly as today

#### Scenario: Retained RO under FULL never widens
- **WHEN** a read-only target sits at or under a FULL route (or its source under a FULL source) and an extension declares `Full` rights on it
- **THEN** the Podman binding stays read-only (the VFS binding carries what the union cannot subtract); writes fail at the filesystem layer

#### Scenario: Same-target driver RO plus extension Write stays VFS RO
- **WHEN** a driver declares `H->/mirror:ro` with `/mirror` under an effective FULL target or source, and an extension requests `Write` at `/mirror`
- **THEN** Landlock grants `Full` (joined minimum) while the Podman binding stays read-only (retention reads the surviving binding intent); writes fail at the filesystem layer — the join widened the grant without erasing the wall

#### Scenario: Sugar decomposes once into the canonical IR
- **WHEN** a profile declares a mount with `mode = "ro"` and no explicit `[[permissions]]` entry for its target
- **THEN** the in-memory plan carries one placement plus one `Read` grant plus read-only intent — never a doubled entry — and rendering proceeds exactly as for the explicit triple

#### Scenario: Supplements cannot smuggle permission overrides
- **WHEN** a `[supplements]` table carries a permission-shaped key for a bound target
- **THEN** framework validation refuses typed; policy travels only through `[[permissions]]` and sugar, never opaque knobs

#### Scenario: Write-only normalizes before retention
- **WHEN** an extension declares bare `Write` on `/state` while the same dentries are `Read`-bound at `/mirror` under a retained-RO driver declaration
- **THEN** every consumer classifies `/state` as effective `Full` (its host source joins FULL-source accounting despite the `Write` label), `/mirror` retains read-only or refuses per the alias preflight, `/state` binds read-write, and the original `Write` label survives only in provenance records
