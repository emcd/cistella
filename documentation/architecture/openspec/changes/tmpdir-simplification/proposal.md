## Why

`/tmp/scratch` is day-one original sin (present in the first driver commit, never deliberated): a writable outlet beside a denied `/tmp`, kept alive through Landlock by `TMPDIR` redirections instead of ever questioning the denial. Both shapes are host-backed directories with carveouts — the outlet was never safer, only older. With the tmpdir extension proven, the simplification is available: framework and isolator know nothing about `/tmp`, Landlock grants it unconditionally, and the extension owns the host-backed slice. Deletions outnumber additions.

## What Changes

- Delete the scratch apparatus: the unconditional `/tmp/scratch` triple (`main.rs`), `TMPDIR`/`BUN_TMPDIR` redirections in the remaining fleet profiles (maintainer already clean), teardown residue legs for scratch paths, outlet docs. The generic mountpoint pre-create stays scoped (a user bind under a slice is a valid admitted shape); `nested_ro_preflight` (generic RO-ancestor mechanism) stays.
- Landlock grants FULL on guest `/tmp` unconditionally, whatever backs it (image rootfs, host slice). The admitted-vs-undeclared `/tmp` distinction and its carveout-derivation branches delete; denial-matrix tests repin from denied to admitted-host (sibling-denial evidence unchanged — it comes from outside the target).
- Tmpdir extension, unconditional: always mints a slice under host `/tmp` (parent-selection logic — `$TMPDIR`/XDG fallback — collapses to the hardcoded parent), contributes it as RW `/tmp`, declares cleanup. No knobs, no conditions; declared profiles get exactly this.
- Specs: `src-confinement` (`/tmp` always FULL qualified by writable backing + RO guarantees + baseline alias accounting), `tmpdir-service` (unconditional host-`/tmp`), `mounts` (scratch + nested-RO wording), `extension-protocol` (session-id scope wording). No `framework-lifecycle` delta (pre-create mechanism stays).
- Live tests rework to the simplified shapes (no outlet probes, no registry remnants); fast pins for the grant and the hardcoded parent.

## Capabilities

### New Capabilities

(none — behavior converges onto fewer mechanisms, no new surface.)

### Modified Capabilities

- `src-confinement`: `/tmp` FULL grant unconditional under hook qualified by writable backing + retained-RO guarantees; baseline alias accounting over backing sources (RO-elsewhere aliases refuse typed).
- `tmpdir-service`: slice parent always host `/tmp`; vending unchanged (shipped).
- `mounts`: per-session scratch triple scenario removed; nested-RO preflight untouched (scratch wording dropped).
- `extension-protocol`: session-id scope clause drops `scratch`.
- No `framework-lifecycle` delta (pre-create mechanism stays generic-scoped).

## Impact

- Code: deletions dominate (scratch triple + residue legs, parent selection, carveout branches); additions are grant-default + hardcoded parent + baseline alias accounting. The generic pre-create stays.
- Profiles: three fleet files lose two env lines each (mechanical); no extension blocks change.
- Systems: hooked sessions without tmpdir get a writable image `/tmp` (FULL grant) instead of denied+outlet — behavior change, intended. Plain sessions untouched (image `/tmp` as today).
- Non-Goals: composition N-law (separate change, lands on this base); SSH extension; GH artifact rework; feature gates.
