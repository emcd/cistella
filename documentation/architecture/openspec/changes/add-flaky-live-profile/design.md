## Context

The live tier (89 tests in `tests/integration/`, 1 in `tests/unit/lock_teardown.rs`, all `#[ignore = "live: ..."]`) runs on the four-leg CI matrix via `cargo nextest run -P live --run-ignored=all`. Rotating backend weather (podman/systemd user-manager on hosted runners) fails random tests run to run; every observed failure clears in isolation. The gate therefore cannot distinguish product regressions from backend weather. Prior remediation (capture/diagnostic hardening, ASSUME-shape fixes) removed test-side flakiness but cannot fix the backend. Stakeholders: reviewers (gate authority), operator (merge authority), QA (live evidence).

Constraints: no src/ changes; no test deletions; no rerun-to-green; per-test reviewer approval for any gate move; roster proof by execution at the exact tip.

## Goals / Non-Goals

Goals: run every live test every run (nothing deleted, nothing skipped silently); restore a meaningful gate by excluding only explicitly approved tests; make failures in the excluded set visible and tracked; make roster membership auditable from CI output.

Non-goals: fixing any specific flake (separate per-test work); changing what the tests assert; preapproving any move (initial set is empty); product behavior changes.

## Decisions

### 1. Selection by explicit name roster, not by ignore reason

The roster is a versioned text file (one fully-qualified test path per line) under `.auxiliary/configuration/`. CI builds two nextest `-E` expressions from it: an inclusion filter for the flaky leg (`test(<name>)` joined by `or`), an exclusion filter for the gating legs (`not (<inclusion>)`). Verified against nextest 0.9 at this tip: full test names as shown by `nextest list` (module path without binary prefix) are unique across the workspace, and no live-gated name is a substring of any other test name — so plain `test(<full-name>)` matches exactly one test, with no regex anchoring needed. Leaf-only names are forbidden (one leaf collides across two binaries today). The roster check re-verifies uniqueness implicitly: a rename surfaces as a membership/count failure. Generation is mechanical; exactness is proven by `nextest list` output, never asserted from the file.

Alternatives: selecting by `#[ignore]` reason — rejected, the nextest CLI cannot select on the ignore string; moving tests to quarantine files — rejected, churns history and renames break the roster anyway; deleting flaky tests — rejected, coverage loss the reviewers forbid.

### 2. A named `flaky` profile mirroring `live`

`[profile.flaky]` in `nextest.toml` repeats the `live` overrides (per-profile override rule, including the user-manager group). The profile documents intent and gives future tuning a home (e.g., different parallelism for known-flaky backend interactions). The workflow invokes it with `--run-ignored=all` plus the inclusion filter.

### 3. Roster proof as a CI step, not a human check

A roster-check step compares `cargo nextest list --message-format json` output for both selections at the exact tip: intersection must be empty, union must equal the full live universe (90 today). Failure fails the workflow loudly. Test renames surface as union gaps, not silent omissions — drift fails closed.

### 4. Empty initial roster

The mechanics land with zero quarantined tests: gating behavior is bit-identical, the flaky leg runs an empty selection (verified during implementation; fallback is a parse-only assertion if nextest errors on empty selection). Every future move is a per-test packet with the decision row the reviewers require. This keeps the gate-change review honest: mechanics now, membership later.

### 5. Informational leg semantics

The flaky leg never gates. Its failures are retained in logs, recorded against the owning issue with run ID and raw output, and never retried to green. Cleanup allowance and diagnostics rules are unchanged from the gating legs.

## Risks / Trade-offs

- [Roster drift on rename] → the union check fails the workflow; the fix is a one-line roster update in the same packet as the rename.
- [Filter-syntax edge cases] → test names are exact paths; the `-E` expression is generated, not hand-written, and proven by `nextest list` output before any test executes.
- [Empty-selection behavior] → verified during implementation; a parse-only fallback keeps the leg meaningful.
- [Two legs cost runner time] → the flaky leg runs only the quarantined set (empty today); cost scales with actual quarantine, which is the honest price of the coverage.

## Migration Plan

Land mechanics + empty roster in one packet (no behavioral gate change; rollback is revert). Then per-test quarantine packets as evidence accrues. Reentry reverses a row with fix + green-run evidence.

## Open Questions

(none open - resolved during design: the `-E` syntax is plain `test(<full-name>)` joined by `or`, with `not (...)` for exclusion, verified against nextest 0.9; the empty-selection exit code is verified during implementation with a parse-only fallback.)
