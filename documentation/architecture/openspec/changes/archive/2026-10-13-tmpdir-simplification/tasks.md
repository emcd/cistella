## 1. Delete the scratch apparatus

- [x] 1.1 Remove the unconditional scratch triple + teardown residue legs for scratch paths (including legacy scratch paths and `seat_temp_root` TMPDIR-awareness — no backcompat, pre-1.0); `TMPDIR`/`BUN_TMPDIR` removal from agentmux/coordinator/opencode REVERTED 2026-10-13 — those seats run installed 0.2 (deny+outlet) without tmpdir, so the outlet stays until post-0.3 cutover (maintainer canary only).
- [x] 1.2 Keep generic mountpoint pre-create (scoped, no framework-`/tmp` special case); add user-nested-bind acceptance pin (slice plus `/tmp/cache` bind: mountpoint seeded seat-owned, reap clean).

## 2. Unconditional tmpdir + grant

- [x] 2.1 Guest: hardcode slice parent to host `/tmp` (drop selection); no knobs. Unit test asserts the `/tmp` prefix (host slices, gc-reaped).
- [x] 2.2 Landlock: FULL grant on guest `/tmp` unconditional; remove admitted-vs-undeclared branches; extend effective-FULL accounting + alias preflight to baseline backing sources (RO-elsewhere aliases of `/tmp`-backed dentries refuse typed, ancestor-covered included); repin denial-matrix tests (admit-host with ownership checks; sibling/graft/alias denials stay).
- [x] 2.3 Pins (fast + live): duplicate-RO-alias retention, sibling-source RO-at-`/tmp` refusal, ancestor-covered refusal, user-nested-bind acceptance; dentry protections (I2/I4/I8) preserved; new hooked proofs execute the wrapped harness lineage.

## 3. Specs, docs, validate

- [x] 3.1 Deltas: `src-confinement` (always-FULL qualified by writable backing + RO guarantees), `tmpdir-service` (unconditional host-`/tmp`), `mounts` (scratch + nested-RO wording), `extension-protocol` (scope wording) — no `framework-lifecycle` delta (pre-create mechanism stays); image-`/tmp` read audit (grep + tier-2 Dockerfile findings + live read-probe) BEFORE live conduct; new hooked proofs execute the wrapped harness lineage (plain podman exec is outside the guarantee).
- [x] 3.2 Fast green both umasks (558/558); clippy, fmt, openspec validate (16/16) clean; host live tier via QA 634/634 + targeted 11/11 + canary read-probe clean (2026-10-13).
