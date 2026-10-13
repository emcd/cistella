## 1. Delete the scratch apparatus

- [ ] 1.1 Remove the unconditional scratch triple + teardown residue legs for scratch paths (including legacy scratch paths and `seat_temp_root` TMPDIR-awareness — no backcompat, pre-1.0); remove `TMPDIR`/`BUN_TMPDIR` from agentmux/coordinator/opencode profiles (maintainer already clean).
- [ ] 1.2 Keep generic mountpoint pre-create (scoped, no framework-`/tmp` special case); add user-nested-bind acceptance pin (slice plus `/tmp/cache` bind: mountpoint seeded seat-owned, reap clean).

## 2. Unconditional tmpdir + grant

- [ ] 2.1 Guest: hardcode slice parent to host `/tmp` (drop selection); no knobs. Unit test asserts the `/tmp` prefix (host slices, gc-reaped).
- [ ] 2.2 Landlock: FULL grant on guest `/tmp` unconditional; remove admitted-vs-undeclared branches; extend effective-FULL accounting + alias preflight to baseline backing sources (RO-elsewhere aliases of `/tmp`-backed dentries refuse typed, ancestor-covered included); repin denial-matrix tests (admit-host with ownership checks; sibling/graft/alias denials stay).
- [ ] 2.3 Pins (fast + live): duplicate-RO-alias refusal, sibling-source RO-at-`/tmp` refusal, user-nested-bind acceptance; dentry protections (I2/I4/I8) preserved; new hooked proofs execute the wrapped harness lineage.

## 3. Specs, docs, validate

- [ ] 3.1 Deltas: `src-confinement` (always-FULL qualified by writable backing + RO guarantees), `tmpdir-service` (unconditional host-`/tmp`), `mounts` (scratch + nested-RO wording), `extension-protocol` (scope wording) — no `framework-lifecycle` delta (pre-create mechanism stays); image-`/tmp` read audit (grep + tier-2 Dockerfile findings + live read-probe) BEFORE live conduct; new hooked proofs execute the wrapped harness lineage (plain podman exec is outside the guarantee).
- [ ] 3.2 Fast green both umasks; clippy, fmt, openspec validate; host live tier via QA (tmpdir shapes without outlet, hooked image-`/tmp` admission).
