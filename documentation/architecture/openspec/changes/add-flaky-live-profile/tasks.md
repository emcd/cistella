## 1. Roster and profile

- [x] 1.1 Add `.auxiliary/configuration/cistella/test-exclusions.toml` with an empty `quarantined` array and a header comment stating the format and the empty-set semantics
- [x] 1.2 Add `[profile.flaky]` to `.auxiliary/configuration/nextest.toml` mirroring the `live` overrides (user-manager group)
- [x] 1.3 Verify `-E` inclusion/exclusion syntax against installed nextest 0.9 via `cargo nextest list` (including the empty-selection exit status; record the fallback if needed)

## 2. CI wiring

- [x] 2.1 Add roster resolution to `tester.yaml` AND `releaser.yaml`: build inclusion and exclusion `-E` expressions from the TOML roster (both workflows gate on the live suite today)
- [x] 2.2 Apply the exclusion expression to the gating live legs
- [x] 2.3 Add the informational flaky leg (non-gating, runs inclusion expression, failures retained in logs)
- [x] 2.4 Add the roster-check step: strict TOML parsing; independent `--ignore-default-filter` baseline (no profile) by composite identity; per-predicate verification via real `nextest list -E` under both tier profiles (exactly one live-eligible identity; reject duplicates/zero/overmatch/non-full-name); informational-equals-roster check; disjointness; union against the full pre-change gating selection (ignored and non-ignored); negative substring-collision and default-filter-omission handling
- [x] 2.5 Add committed backend-free controls (`roster-check.py --self-test`): schema/malformed/duplicate fixtures, stale/overmatch/ordinary-collision/non-live/leaf fixtures
- [x] 2.6 Prove roster accounting on every matrix leg (gating and flaky, qa and release) before its selections are used
- [x] 2.7 Flaky leg discriminates completion from infra failure (no continue-on-error; missing Summary fails loud; `--no-tests pass` only on proven-empty roster; `--no-fail-fast`; full log uploaded as artifact); manual dispatch filter intersects both tiers

## 3. Verification

- [x] 3.1 `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `openspec validate --all --strict` clean at the tip
- [x] 3.2 Fast suite green; roster proof demonstrated at the exact tip (list output showing disjoint selections covering all 90)
- [ ] 3.3 Targeted branch CI run green (gating legs unaffected with empty roster; flaky leg passes vacuously)
- [ ] 3.4 Route the packet through tier-1, then tier-2 (no gate moves in this packet; membership changes are separate per-test packets)

## 4. R1-R3 follow-up (tier-2 M1-M4 implementation review findings and tier-1 F1-F4)

- [x] 4.1 Raw-set predicate validation on the full discovered set (ordinary-test collisions cannot hide behind profiles)
- [x] 4.2 Command-builder pins: baseline bypasses tier/roster/default without a profile; tier listings carry -P and no bypass; execution build flags everywhere
- [x] 4.3 Backend-free self-test fixtures for all of the above plus decide-flaky anchoring and effective-subset computation; --self-test wired as a required CI step
- [x] 4.4 Effective diagnostic subset computed in-leg with omitted identities named; full-audit dispatch route implemented and documented
- [x] 4.5 F1: gating suite.log uploaded as artifact with if: always() on both workflows
- [x] 4.6 F3: recorded argv asserted per production listing call (bypass/profile composition pinned, not just the constructor)
- [x] 4.7 F4: dead duplicate cmd_effective_subset removed; single implementation retained and tested
- [x] 4.8 F2: audit-failure policy decision — honest reds, no advisory status (operator decision 2026-10-25; auditor concurring guidance)
