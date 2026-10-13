## Context

`tempdir-extension` proved the `/tmp` protocol with a reference guest that was deliberately scoped as a test fixture: source under `tests/fixtures/`, built as a cargo `[[example]]`, resolved by tests through a target-dir glob and through test-harness registries. The operator intends production use as a second shipped extension (distributed and fleet-installed like the Landlock extension — both resolve hardcoded, registry stays third-party-only), with the maintainer seat as canary. Current state constraints: sibling-relative discovery requires the guest beside the driver binary; profile changes apply at conduct (re-conduct to take effect); everything referencing the old names is test-only.

## Goals / Non-Goals

**Goals:**
- Ship `cistella-extension-tmpdir` as a first-class sibling binary with zero protocol/behavior change.
- One spelling (`tmpdir`) across binary, role, profiles, tests, docs, specs.
- Supervisor naming pattern established (`cistella-supervisor-landlock`).
- Maintainer canary wired at the profile level, rollback-safe.

**Non-Goals:**
- No guest protocol, merge, reap, or Landlock-rule changes (byte-identical behavior; digests change only via the role-default string).
- No host registry entries (operator action, prerequisite to the canary).
- No other fleet profiles (canary first; rollout follows proof).
- No GH single-file artifact rework (pre-existing gap, unchanged).
- No cargo feature gates (deferred, `cistella:todos/45`).

## Decisions

- **`tmpdir` over `tempdir`.** The canonical environment variable is the deciding vote; consistency was the requirement. Full alignment including the spec directory (`tmpdir-service`), while anything is still cheap.
- **Move, don't rewrite, the guest.** The reference implementation is proven (620/620 + live dogfood); the promotion changes its address and role default, not its logic. Review burden stays on the move + rename, not re-verification.
- **Peer resolution follows the shipped-guest pattern.** Tests globbing `target/*/examples/` switch to sibling-relative release binaries (current-exe dir), exactly how `example_binary` already resolves non-tempdir peers... tempered: keep each resolver's existing selection semantics (canonical vs newest-mtime), changing only the directory + the role string. No cross-resolver unification in this change (unrelated refactor).
- **Supervisor rename rides along.** Same review cycle, same mechanical shape (binary + staged guest path + constant + docs + tests). The staged guest path moves from `/run/cistella/hooks/landlock-wrap` to `/run/cistella/hooks/supervisor-landlock` (guest-visible path — profiles do not reference it, but live sessions in flight across an upgrade would mismatch; upgrade is re-conduct, no live migration).
- **Canary is profile-only.** This change edits `cistella-maintainer.toml` (add extension, drop `TMPDIR`/`BUN_TMPDIR`); the binary install and the re-conduct are operator actions with revert-file rollback. The unconditional `/tmp/scratch` framework triple stays (outlet for non-tmpdir sessions — not a workaround, load-bearing).
- **Shipped resolution, not registry.** Two vended extensions resolving two different ways is incoherent; the registry stays third-party-only (proven sufficiently by the `example_guest` integration test per operator direction). `tmpdir` hardcodes beside `landlock` in `resolve_extensions` with a `Tmpdir` spawn mirroring the landlock sibling spawn (install-dir trust, no digest verification — same trust as the landlock sibling; a missing sibling fails closed at discovery). The v1 pair rule (`at most one non-landlock`) is untouched: landlock+tmpdir is exactly the admitted pair.
- **Digest re-pin friction is documented, not solved.** Each host rebuild changes guest bytes → registry re-pin. Acceptable for one canary seat; packaging (todos/42) is the real fix.

## Risks / Trade-offs

- [Risk] Role rename misses a reference (test registry, doc, profile) → Mitigation: `tempdir` grep must read empty outside archive/ at commit time (archive/ records are historical and keep the old spelling deliberately); the fast suite pins role strings in registry/merge tests.
- [Risk] Canary bricks the maintainer seat (broken profile or uninstalled sibling at re-conduct) → Mitigation: rollback is one file revert + re-conduct; validate profile parse (`cistella check`-class validation) before re-conduct; operator keeps a known-good shell outside the seat during the switch.
- [Risk] Staged-path rename strands in-flight hooked sessions across upgrade → Mitigation: none needed beyond re-conduct discipline (no live migration supported anywhere in the lifecycle); noted here so the upgrade runbook says it.
- [Risk] Digest churn on every host rebuild → Mitigation: not applicable (no digest verification on the shipped path — install-dir trust, same as landlock); registry digest pins remain third-party-only.
- [Risk] Single-file GH artifact consumers get a driver without its new sibling → Mitigation: pre-existing gap (same true of landlock today), explicitly out of scope; fleet installs from source/cargo.

## Migration Plan

1. Code move + renames + test-infra switch (reviewed, fast-green both umasks).
2. Spec deltas + main rename (`tempdir-service` → `tmpdir-service`), docs, fixture profiles.
3. Profile edit for the canary ONLY after the shipped binary is installed beside the driver (resolve hardcodes `tmpdir`; an uninstalled sibling fails closed at spawn — the edit is live, not inert). Order inside the window: install → edit → validate → re-conduct.
4. Operator: install → validate profile → re-conduct maintainer seat → observe (tools use stock `/tmp`; slice minted, reaped) → rollout to remaining profiles or revert-file rollback.

## Open Questions

- (resolved) Registry entry name: moot — no registry entry (shipped resolution).
- (resolved) 0.3.0 scope: part of the 0.3.0 cycle per operator.
