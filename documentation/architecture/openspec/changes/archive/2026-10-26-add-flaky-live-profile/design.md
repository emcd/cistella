## Context

The live tier (89 tests in `tests/integration/`, 1 in `tests/unit/lock_teardown.rs`, all `#[ignore = "live: ..."]`) runs on the four-leg CI matrix via `cargo nextest run -P live --run-ignored=all`. Rotating backend weather (podman/systemd user-manager on hosted runners) fails random tests run to run; observed stochastic failures clear in isolation and known harness bugs were corrected, while residual causation for the rotation is unresolved. The gate therefore cannot distinguish product regressions from backend weather. Prior remediation (capture/diagnostic hardening, ASSUME-shape fixes) removed the identified test-side flakiness; residual CI reds remain open. Stakeholders: reviewers (gate authority), operator (merge authority), QA (live evidence).

Constraints: no src/ changes; no test deletions; no rerun-to-green; per-test reviewer approval for any gate move; roster proof by execution at the exact tip.

## Goals / Non-Goals

Goals: run every live test every run (nothing deleted, nothing skipped silently); restore a meaningful gate by excluding only explicitly approved tests; make failures in the excluded set visible and tracked; make roster membership auditable from CI output.

Non-goals: fixing any specific flake (separate per-test work); changing what the tests assert; preapproving any move (initial set is empty); product behavior changes.

## Decisions

### 1. Selection by explicit name roster, not by ignore reason

The roster is the TOML array `quarantined` in `.auxiliary/configuration/cistella/test-exclusions.toml` (one full test name per string, exactly as `nextest list` shows it). CI builds two nextest `-E` expressions from it in both the `qa` (`tester.yaml`) and `release` (`releaser.yaml`) workflows — the release workflow gates publication on the same live suite, so exempting only `qa` would leave release tags gating on quarantined tests. The inclusion filter feeds the flaky leg (`test(<name>)` joined by `or`), an exclusion filter for the gating legs (`not (<inclusion>)`). Verified against nextest 0.9 at this tip: full test names as shown by `nextest list` (module path without binary prefix) are unique across the workspace, and no live-gated name is a substring of any other test name — so plain `test(<full-name>)` matches exactly one test, with no regex anchoring needed. Leaf-only names are forbidden (one leaf collides across two binaries today). The roster check re-verifies uniqueness implicitly: a rename surfaces as a membership/count failure. Generation is mechanical; exactness is proven by `nextest list` output, never asserted from the file.

Alternatives: selecting by `#[ignore]` reason — rejected, the nextest CLI cannot select on the ignore string; moving tests to quarantine files — rejected, churns history and renames break the roster anyway; deleting flaky tests — rejected, coverage loss the reviewers forbid.

### 2. A named `flaky` profile mirroring `live`

`[profile.flaky]` in `nextest.toml` repeats the `live` overrides (per-profile override rule, including the user-manager group). The profile documents intent and gives future tuning a home (e.g., different parallelism for known-flaky backend interactions). The workflow invokes it with `--run-ignored=all` plus the inclusion filter.

### 3. Roster proof as a CI step, not a human check

A roster-check step compares `cargo nextest list --message-format json` output at the exact tip. It first builds the independent baseline with `--ignore-default-filter` and no profile flag, identifying tests by composite identity (binary plus full test name); then it resolves both tier selections under the legs' actual profiles and flags (`-P live` gating, `-P flaky` informational, no bypass). It asserts, in order: (1) each roster predicate resolves via a real `nextest list -E` selection to exactly one live-eligible composite identity, with duplicates, duplicate lines, zero matches, and overmatches rejected; (2) the informational selection equals the roster in membership and count; (3) the tiers are disjoint; (4) their union plus the preserved ordinary (non-ignored) tests equals the full pre-change gating selection. Failure fails the workflow loudly — the check itself is a required gate, and every matrix leg re-proves it on its own platform before its selections are used, retaining per-platform evidence. Test renames surface as union gaps, not silent omissions; a default-filter that hides a test surfaces as a baseline mismatch — drift fails closed either way. A negative fixture (a roster-adjacent ordinary-test name that must not match) pins the substring-collision rule; a default-filter-omission fixture pins the baseline independence. The `roster-check.py --self-test` suite pins the pure rules (schema, duplicates, stale, overmatch, ordinary-collision, leaf rejection) without cargo.

### 4. Empty initial roster

The mechanics land with zero quarantined tests: gating behavior is bit-identical, the flaky leg runs an empty selection (verified during implementation; fallback is a parse-only assertion if nextest errors on empty selection). Every future move is a per-test packet with the decision row the reviewers require. This keeps the gate-change review honest: mechanics now, membership later.

### 5b. Full live audit route

Dispatching the qa workflow with `full_audit: true` runs the complete intended live set on a dedicated `audit` job (one per matrix leg) with no quarantine exclusion and `--no-fail-fast`, so the executed union is the whole universe and every live-gated test actually executes on each platform. The audit job has a distinct check name (`audit ...`), so audit reds can never be mistaken for gating reds, and branch protection must not require it: it is informational by invocation, and normal gating policy is unchanged. A red audit run fails here — that is the honest signal, not a suppression — and per-leg logs (gating suite.log, flaky flaky.log, audit audit.log, diagnostic diagnostic.log, all uploaded as artifacts) are retained as the audit evidence. Required-check identity is invariant: `test (platform)` ALWAYS means the full live-minus-roster selection regardless of dispatch inputs; a manual `filter` dispatch never narrows the required gate. Filtered gating subsets run in the distinct `diagnostic` job (manual AND exclusion, check name `diagnostic (platform)`), and the flaky leg intersects inclusion AND manual under its own `flaky (platform)` name; the effective subset and any omitted roster identities are computed in the leg and the decide step fails loud on an unexpected empty selection.

### 5. Informational leg semantics

The flaky leg never gates — but it runs the full four-leg matrix, so every quarantined test still executes on each formerly covered OS/arch. Platform coverage is preserved by default; a per-test decision may explicitly accept a platform reduction with the same explicit approval as any other coverage delta. There is no continue-on-error anywhere on the leg: instead the test step delegates to the tested `decide-flaky` classifier, which discriminates — it exits non-zero only when the run did not complete (infrastructure, config-parse, discovery, or build failure, evidenced by a missing final Summary) or when the selection is unexpectedly empty despite a non-empty roster. Completed runs, green or red, exit zero; reds are recorded from the retained log (uploaded as an artifact on every leg), never retried to green. `--no-tests pass` applies only when the roster is proven empty; `--no-fail-fast` ensures every quarantined test executes or is explicitly recorded. A manual dispatch filter intersects the flaky tier (inclusion AND manual), which with the `diagnostic` job is the documented route for deliberately targeting subsets including quarantined tests. The required `test` legs never honor the manual filter: `test (platform)` always attests to the full live-minus-roster selection, and filtered gating subsets execute only in the distinct `diagnostic` job (never skipped, never under a required name). The `roster-check.py --self-test` suite pins the parser, predicate validation, set accounting, filter generation, and decision-table rules without cargo. Cleanup allowance and diagnostics rules are unchanged from the gating legs.

## Risks / Trade-offs

- [Roster drift on rename] → the union check fails the workflow; the fix is a one-line roster update in the same packet as the rename.
- [Filter-syntax edge cases] → test names are exact paths; the `-E` expression is generated, not hand-written, and proven by `nextest list` output before any test executes.
- [Empty-selection behavior] → verified during implementation; a parse-only fallback keeps the leg meaningful.
- [Two legs cost runner time] → the flaky leg runs only the quarantined set (empty today); cost scales with actual quarantine, which is the honest price of the coverage.

## Migration Plan

Land mechanics + empty roster in one packet (no behavioral gate change; rollback is revert). Then per-test quarantine packets as evidence accrues. Reentry reverses a row with fix + green-run evidence.

## Open Questions

(none open - resolved during design and verified in implementation: the `-E` syntax is plain `test(<full-name>)` joined by `or`, with `not (...)` for exclusion, verified against nextest 0.9; an empty selection runs zero tests and exits 0 via `--no-tests pass`, with the roster check as the proof the set is genuinely empty.)
