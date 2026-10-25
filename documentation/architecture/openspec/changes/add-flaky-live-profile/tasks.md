## 1. Roster and profile

- [x] 1.1 Add `.auxiliary/configuration/cistella/test-exclusions.toml` with an empty `quarantined` array and a header comment stating the format and the empty-set semantics
- [x] 1.2 Add `[profile.flaky]` to `.auxiliary/configuration/nextest.toml` mirroring the `live` overrides (user-manager group)
- [x] 1.3 Verify `-E` inclusion/exclusion syntax against installed nextest 0.9 via `cargo nextest list` (including the empty-selection exit status; record the fallback if needed)

## 2. CI wiring

- [x] 2.1 Add roster resolution to `tester.yaml` AND `releaser.yaml`: build inclusion and exclusion `-E` expressions from the TOML roster (both workflows gate on the live suite today)
- [x] 2.2 Apply the exclusion expression to the gating live legs
- [x] 2.3 Add the informational flaky leg (non-gating, runs inclusion expression, failures retained in logs)
- [x] 2.4 Add the roster-check step: independent `--ignore-default-filter` baseline by composite identity; per-predicate raw-set validation (exactly one live-eligible identity; reject duplicates/zero/overmatch); informational-equals-roster check; disjointness; union against the full pre-change gating selection (ignored and non-ignored); negative substring-collision and default-filter-omission fixtures

## 3. Verification

- [ ] 3.1 `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `openspec validate --all --strict` clean at the tip
- [ ] 3.2 Fast suite green; roster proof demonstrated at the exact tip (list output showing disjoint selections covering all 90)
- [ ] 3.3 Targeted branch CI run green (gating legs unaffected with empty roster; flaky leg passes vacuously)
- [ ] 3.4 Route the packet through tier-1, then tier-2 (no gate moves in this packet; membership changes are separate per-test packets)
