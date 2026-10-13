# src-confinement Specification

## Purpose
Source confinement: synced from change tmpdir-simplification. Update Purpose after archive.
## MODIFIED Requirements
### Requirement: Guest-visible topology translation

Landlock rules SHALL apply to guest-visible paths, not host pathnames: conduct SHALL translate the canonical host ancestor/subtree through the validated guest mount topology to every guest-visible route before restricting. Profiles with UNTRANSLATABLE aliases (symlinked ancestors with no guest route, undescribable shapes) SHALL refuse pre-create rather than confine partially. A second bind mount into the same sibling that DOES translate admits or denies BY DECLARED MODE: declared read-write grafts compose into full-rights carveouts (accounted, admitted), while second routes with no declared carveout stay denied. R+X ancestor plus full-rights subtree is correct union semantics only within one verified layer and a complete path view. Guest `/tmp` SHALL always carry a FULL-rights carveout under hook, whatever backs it (image rootfs, host slice) — admitted by construction, never by triple. The carveout is Landlock permission only: retained Podman read-only bindings and filesystem ownership modes still enforce, Baseline alias accounting: a host source backing `/tmp` joins FULL-source accounting (every RO route to its dentries retains VFS read-only); a covering triple that is read-only declared yet unretained (notably ancestor bindings flipped for materialization) refuses pre-create typed. There is no undeclared-`/tmp`-denied state.

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

#### Scenario: Admitted /tmp writes on writable backing
- **WHEN** a hooked session's harness writes inside `/tmp` (image rootfs or tmpdir host slice alike) where the backing and its permissions admit writes
- **THEN** the write succeeds (FULL Landlock permission by construction — permission, never filesystem enforcement); denial evidence comes only from outside the target

#### Scenario: Retained RO declarations still deny
- **WHEN** a `/tmp` backing or child carries a retained Podman read-only declaration or denying ownership mode
- **THEN** writes fail at the filesystem layer despite the FULL Landlock permission; the baseline never overrides read-only bindings or DAC

#### Scenario: Sibling denial beside an admitted /tmp
- **WHEN** a hooked session admits `/tmp` and the harness writes outside it to a sibling subtree path
- **THEN** the sibling write fails with `EACCES` (the FULL directory rule on `/tmp` covers its descendants; denial evidence comes from outside the admitted target)
