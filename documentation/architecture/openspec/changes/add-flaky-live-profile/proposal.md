## Why

The live tier fails stochastically on rotating backend interactions (podman/systemd user-manager on CI runners), documented in `issues/17` and confirmed across branch-CI runs in October 2026. Every failure in that stochastic class clears in isolation and none repeats deterministically — the deterministic orphan failure was a distinct, fixed test bug, not backend weather — so the gate cannot distinguish a product regression from backend weather, and re-running to green proves nothing. Coverage cannot simply be deleted either: the flaking tests pin real failure modes. A quarantine tier preserves execution and evidence while restoring a meaningful gate signal.

## What Changes

- A new `flaky` nextest profile plus CI wiring that runs quarantined live tests as an informational, non-gating leg. Nothing is deleted; every live test still executes somewhere.
- Gating live legs exclude exactly the quarantined set; the flaky leg runs exactly the quarantined set. Selection is by explicit test-name roster with execution proof, not by ignore-reason.
- A single source of truth for the roster plus accounting rules: gating and informational sets disjoint, union equals the full live universe (currently 90: 89 in `tests/integration/`, 1 in `tests/unit/lock_teardown.rs`).
- Quarantine criteria and a per-test decision process: quarantine is per-test, evidence-driven, reviewer-approved, with a named owner, issue, and reentry condition. "Move on next flake" is not preapproval; an empty initial set is valid.
- Initial quarantine set: empty. The orphan deterministic failure has not reproduced since its causal fix (targeted 2/2 green on all four legs across four branch-CI runs at the fix tips; targeted evidence only, not full-live); the surgery-staging tests stay gating per reviewer direction until their staging is fixed in place.

## Capabilities

### New Capabilities

- `flaky-live-quarantine`: quarantine-tier mechanics for live-gated tests — roster source of truth, set accounting (disjoint/union proof), gating vs informational execution, per-test move/reentry process, failure visibility rules.

### Modified Capabilities

(none — no product behavior changes; test-execution topology only)

## Impact

- `.auxiliary/configuration/cistella/test-exclusions.toml`: quarantine roster (`quarantined` array, empty initially) + nextest `flaky` profile; `tester.yaml` and `releaser.yaml`: gating `-E` exclusions + informational flaky leg on all four matrix legs (required roster-check gate in both workflows).
- No `src/` changes. No product API, CLI, or spec changes.
- CI signal change (intended): gating legs stop failing on quarantined backend weather; the informational leg surfaces it instead, tracked, never retried to green.
