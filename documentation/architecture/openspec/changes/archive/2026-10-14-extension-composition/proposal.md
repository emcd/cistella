## Why

The v1 pair limit (landlock+tmpdir only) blocks the SSH extension, which needs landlock+tmpdir+ssh in one session. Multi-extension merge order and conflict semantics must be designed before SSH lands, not during.

## What Changes

- **N-extension merge (N-law):** per-plan capability gates run pre-concat; contributions concatenate in declaration order; a single atomic central merge produces the session plan with per-name provenance. Admission refuses when more than one plan contributes guest-hooks AND when one plan contributes more than one hook request (the wrapper chain is the only true ceiling). Contribution collisions refuse deterministically, naming ALL claimants. Duplicate extension declarations refuse at resolution. Landlock keeps NO merge privilege (shipped-vs-registered is resolution-only).
- **Hooks registry:** three lifecycle phases — `prepare` (many; mounts/env/permissions IO after the split), `inject-supervisor` (AT MOST ONE; double claimants refuse typed naming both), `finalize` (many; declarations-plus-observation mechanism, NO new guest spawn — live-guest finalize needs its own threat model) — plus a post-prepare, pre-isolation **compile** phase (AT MOST ONE claimant; frozen admitted snapshot in, rendered bindings out). The compiler owns domain rendering (today: the RO→RW materialization revision relocates out of the framework into the landlock-owned compiler); the framework owns snapshot-freeze, output bounds, and floor enforcement. This is the generality principle in action: no isolator/extension-specific knowledge in framework code.
- **Config [[permissions]] with sugar (weaves `cistella:todos/38`):** a new config `[[permissions]]` array (path + rights, framework-defined closed vocabulary, framework-validated) as the policy layer every confinement-capable isolator/extension consumes; `[[mounts]]` keeps composition. Permissions written on mounts in config are sugar decomposing into the canonical in-memory IR early (ro→placement + `Read` grant + RO binding intent; rw→placement + `Full` grant + writable intent). Misc extension knobs stay in `[supplements]` (opaque, extension-defined) and cannot smuggle permission overrides around the central vocabulary. A permissions-only isolator (no mounts — e.g. a future Bubblewrap/Landlock isolator) is supported by an empty placement plan or an explicit capability refusal, never accept-then-ignore.
- **Mounts-vs-permissions split:** contributions declare placement (`mounts[]`, modeless) separately from rights (`permissions[]` as `(target, rights)` under a `Rights` lattice — `Read`/`Write`/`Full`, wire `read`/`write`/`full`, joined deterministically with all provenances recorded). A contributed placement with no matching permission refuses, as does an orphan permission; extension-contributed Podman binding modes render from effective rights (`Read`→ro, effective `Full`→rw — `Write` normalizes to `Full` at map construction). One `EffectivePermissions` map, computed once post-merge, is the single source of truth for every consumer (grant computation, alias guards, RO-retention, descendant checks, topology rendering); rights drive Landlock grants only and never widen driver-declared RO VFS floors. Existing profiles translate 1:1.
- **Negotiated wire break:** the protocol epoch moves 1→2, checked via a version gate before body parsing; mismatch refuses deterministically typed in both directions. Pre-1.0 states this plainly: sharp break, no translation shim, no binary harness — hello-layer unit tests with actual v1-vs-2 versions and incompatible bodies cover both refusal directions (operator directive 2026-10-14, superseding the actual-old-binary pins).
- **Per-guest admission hardening:** the per-plan gate extends to credentials and permissions pre-concat (closing the union-set laundering path); policy claims are scoped guest-locally (a guest's claims may target only names it contributed in the same transaction); provenance is host-assigned and carried on every contribution/claim, kept as a multimap through validation (missing provenance is a typed internal refusal, never a guess).
- **Concrete phase order with records-only finalize:** resolve → lease → host → per-guest prepare+gate → collect → admission (at most one nonempty hook set containing at most one hook request — one guest returning two hooks refuses) → concat → atomic merge → lattice → compile → supervisor plan → stage/launch → attestation → converged teardown → finalize, which records already-admitted declarations/outcomes only and creates no actions and no new deletion authority.
- **BREAKING:** the v1 pair rule is removed; the extension contribution shape splits (mounts vs permissions) — pre-1.0, break freely.
- **Re-proof now:** landlock+tmpdir re-proved under the merge pipeline in this change; triple-composition pins land WITH the SSH change (never faked with fixtures).

## Capabilities

### New Capabilities

- `extension-hooks`: hook registry, phase semantics (prepare / compile / inject-supervisor / finalize), claimant rules and refusal shapes.

### Modified Capabilities

- `framework-lifecycle`: merge pipeline (gating → concat → atomic merge → provenance), hook phase dispatch.
- `guest-hosting`: N-extension resolution (pair rule out), duplicate refusal, shipped set semantics.
- `extension-protocol`: contribution shape (mounts[] vs permissions[] split), guest-hooks contribution rule.

- `mounts`: ADDED rendering rule (extension-contributed placements arrive modeless; binding modes render from effective rights through the preserved safe RO→RW revision; retained RO never widens) PLUS config-side sugar decomposition (`[[permissions]]` array, ro/rw sugar rules, coexistence, per-class defaults, supplements fence).
- `src-confinement`: carveout derivation terms move from triple modes to the effective-permissions map (guarantees unchanged), plus relocation equivalence (domain algorithms move to the owning compiler; guarantees pinned across the move).

## Impact

- `src/framework/` merge/prepare path (minus relocated domain logic); landlock-owned compiler module (new); guest binaries' contribution shape; config syntax (`[[permissions]]`, sugar); all fixture profiles (1:1 translation at declaration layer); QA host live tier for re-proof.
- Reviews: RG + Advisor (tier-2 explicitly in scope).

## Open Questions (for design)

- Finalize inputs: is exit disposition visible, or pure observation of converged state? (Lean: observation only; disposition is a future phase's input.)
- Does `inject-supervisor` need the merged environment as well as triples (supervisor argv templating from env)? Currently no consumer — defer until one exists.
- Compile crossing a process boundary later: bounded post-merge render op on the retained guest — future work, only if in-process ownership proves insufficient.
