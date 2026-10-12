## Why

The per-session `/tmp` extension proved out its protocol as a test fixture (`tempdir-extension`, shipped, QA-green 620/620 with live dogfood), but the operator intends it as a production guest distributed like the Landlock extension — while the spec still calls it "example-grade" and defers promotion. The reference binary lives under `tests/fixtures/` (built as a cargo example, never installed), the nomenclature is split (`tempdir` role/spec vs `TMPDIR` canonical env), and the supervisor binary name (`cistella-landlock-wrap`) does not reserve the pattern for future backends (Bubblewrap). Pre-0.3.0 is the last cheap moment: nothing production references the old names yet.

## What Changes

- Promote the reference guest to a shipped binary: move `tests/fixtures/tempdir_guest/main.rs` to `src/bin/cistella-extension-tmpdir.rs`, installed and discovered sibling-relative exactly like `cistella-extension-landlock`. No protocol or behavior change; the guest code moves verbatim (plus the role default, below).
- Align nomenclature to `tmpdir` throughout: registry role, hello default, profile extension names, fixture profiles, test registries, docs, and the `tempdir-service` spec (renamed to `tmpdir-service`). **BREAKING** for the two test-only registries (`tempdir-test` entries) — no production registry exists yet, so the break lands nowhere.
- Rename `cistella-landlock-wrap` to `cistella-supervisor-landlock` (binary, staged guest path, constant, docs, test references), reserving `cistella-supervisor-<backend>`. Implementation-only: no spec names the wrapper.
- Correct the production-grade language: the "fixture-grade / promotion waits" clauses become shipped-guest terms (registry-resolved, digest-pinned, fleet-installed like any third-party guest).
- Test-infra follow-through: tempdir peer resolution switches from the cargo-examples glob to sibling-relative release binaries (like shipped guests); fixture profiles and `profiles.md` reference the new names.
- Maintainer canary wiring (profile side only): `cistella-maintainer.toml` gains the `tmpdir` extension and drops `TMPDIR`/`BUN_TMPDIR` overrides, proving a writable `/tmp` without the scratch outlet on a production seat. The host registry entry itself is operator action (prerequisite, not in this change); other fleet profiles keep workarounds until the canary proves out. Rollback is revert-file plus re-conduct.

## Capabilities

### New Capabilities

(none — no new spec-level behavior; promotion, renames, and wiring only.)

### Modified Capabilities

- `tmpdir-service` (renamed from `tempdir-service`): role string `tempdir` → `tmpdir`; reference-vending requirement rewritten from fixture-grade to shipped-guest terms (sibling-installed, registry-resolved, digest-pinned).
- `src-confinement`: two `tempdir` wordings (`/tmp` carveout requirement body, admitted-slice scenario) → `tmpdir`; no semantic change.

## Impact

- Code: one file move (`tests/fixtures/` → `src/bin/`), role-default constant, wrapper binary + staged-path rename, peer-resolution helpers, fixture profiles.
- Release: new binary flows through the existing sibling-install path (cargo install / source builds, like the landlock extension today); the single-file GH artifact question (guests missing beside a lone `cistella` binary) is pre-existing and out of scope.
- Systems: maintainer seat re-conduct picks up the canary (operator action); registry entry prerequisite (operator action).
- Deferred (tracked `cistella:todos/45`): cargo feature-gating for isolator/extension bins.
