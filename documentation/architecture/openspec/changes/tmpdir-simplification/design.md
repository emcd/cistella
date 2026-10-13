## Context

Temp handling is three layered accidents: (1) day-one per-session scratch (`$XDG_RUNTIME_DIR/cistella/<id>`, `/tmp` fallback) mounted at `/tmp/scratch` with `TMPDIR` redirections, from before confinement existed; (2) Landlock default-deny walling `/tmp` without revisiting (1); (3) the tmpdir extension adding a *second* writable temp (host slice at `/tmp`) beside the first. The operator's model collapses all three: nobody manages temp except the extension (host slice) and the image (rootfs `/tmp`); Landlock admits `/tmp` unconditionally. Current constraints: denial-matrix pins encode `/tmp`-denied (must be repinned, not just edited); `nested_ro_preflight` is generic RO machinery independent of scratch; teardown residue checks name scratch paths; three fleet profiles still carry the env redirections.

## Goals / Non-Goals

**Goals:**
- No framework knowledge of `/tmp` (no triple, no env, no residue legs).
- No isolator special-casing (rootfs `/tmp` assumed).
- Landlock FULL on `/tmp`, unconditional.
- Tmpdir unconditional on host-`/tmp` parent, zero knobs.

**Non-Goals:**
- Composition N-law (separate change, this is its base).
- Changing plain-session `/tmp` (image rootfs, already writable).
- Capacity management for host `/tmp` (operator direction: disk default, revisit on measurement, not on theory).

## Decisions

- **Keep the pre-create mechanism, generic-scoped.** Originally sentenced to deletion with its only customer (scratch-under-slice), reprieved by tier-2 finding: a user bind under a slice (`/tmp/cache`) is a valid admitted shape with no framework `/tmp` special case, and its missing mountpoint would again be podman-created container-root. The scoped mechanism (cleanup roots only, anchored nofollow walk) stays; only the framework-`/tmp` special case goes. New pin covers the user-nested-bind contract. `nested_ro_preflight` stays (HOOKED profiles need RO-ancestor chains regardless of temp).
- **Hardcode the slice parent to host `/tmp`.** Drop `$TMPDIR`/XDG selection from the guest. Rationale: operator directive ("always mount a slice under host /tmp"); one behavior, no environment-dependent shapes. Consequence: fast unit tests mint real host-`/tmp` slices (session-unique names, gc-reaped — same discipline live tests already accept).
- **Admit `/tmp` by construction, not by triple.** The grant derives from the hooked session, not from topology translation — so no profile can misdeclare it and no topology edge case can un-admit it. The unconditional grant is Landlock permission only: retained Podman RO bindings and normal DAC/VFS still deny (a declared-RO backing or RO child refuses at the filesystem layer regardless of the FULL rule).
- **Baseline FULL needs alias accounting, not just an argv flag (tier-2 HIGH).** A FULL rule on `/tmp` roots Landlock rights on its backing dentries (Landlock is mount-agnostic). Two legs: (a) RO-elsewhere aliases of the backing that retention covers (duplicate RO binds — VFS-held read-only on every route) proceed safe; (b) aliases that would flip to Podman RW for submount materialization — notably ancestor bindings covering the backing — refuse pre-create typed, like same-tree grafts. Concretely: the backing source joins `full_grant_sources` (retention holds every RO route VFS-read-only), and a new preflight refuses covering triples that are RO-declared yet unretained. Consistent-FULL aliases proceed. Fast + live pins for duplicate-RO retention, sibling-source RO-at-`/tmp`, ancestor refusal, and user-nested acceptance; existing dentry-aware protections (I2/I4/I8) preserved. This is the one place the simplification adds (rather than removes) a guard. The `undeclared-/tmp-denied` scenarios invert to admitted-host scenarios; sibling denial stays the confinement evidence.
- **Hooked-without-tmpdir gets writable image `/tmp`.** Deliberate behavior change (was: denied + outlet). Rationale: with the outlet gone there is no second option, and denying a container-private overlay dir never protected a host principal — it only broke tools.
- **No legacy cleanup, no backcompat.** Pre-1.0 per operator directive: legacy scratch paths, `seat_temp_root` TMPDIR-awareness, and any migration shims delete outright (gc enumeration roots at host `/tmp`, period). Break freely with releases.
- **Profiles lose env lines, gain nothing.** Maintainer already clean; agentmux/coordinator/opencode drop `TMPDIR`/`BUN_TMPDIR`. No extension blocks change (tmpdir already declared where wanted).

## Risks / Trade-offs

- [Risk] Something reads image-`/tmp` content that tmpdir sessions now mask → Mitigation: audit for reads (writes are proven; reads unexamined) before conduct — grep + a live read-probe on the canary seat.
- [Risk] Host-`/tmp` capacity (large temp spills land on shared disk; fixed PATH is not universally disk — hosts may mount tmpfs) → Mitigation: accepted per operator as tradeoff, no quota gate; record actual host bytes/inode observations from QA/host runs and the canary rather than asserting storage type.
- [Risk] Reviewers read kept pre-create as scope creep → Mitigation: say openly it was sentenced to deletion and reprieved by a valid admitted shape (user bind under slice); the pins test the generic contract, not scratch.
- [Risk] Repinned denial matrix weakens the suite's adversarial posture → Mitigation: sibling-denial + graft-escape + alias refusals all stay (confinement evidence never came from `/tmp`); the repin only moves the `/tmp` assertions from deny to admit-host with identity checks (slice ownership).

## Migration Plan

1. Deletions + grant default + hardcoded parent + alias accounting + new pins (reviewed, fast-green both umasks).
2. Spec deltas (four: `src-confinement`, `tmpdir-service`, `mounts`, `extension-protocol`), docs, fleet profiles.
3. Live re-proof (tmpdir shapes without outlet; hooked image-`/tmp` admission) + canary observation on the already-clean maintainer seat.
4. Push; composition lands on this base.

## Open Questions

- Image-`/tmp` read audit gates live conduct (single coherent order: grep audit incorporating tier-2's Dockerfile findings — builder-stage terminfo staging + validate.sh ephemeral HOME only, no baked read dep — plus a live read-probe on the canary seat for arbitrary harness dependencies, THEN live proofs).
