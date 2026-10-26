## Why

The `conduct_enter_refuses_stale_incarnation_after_restart` live test fails intermittently in CI with two observed signatures — sustained `podman inspect` exit 125 across the 60s landing window, and a fast post-restart gate error from the exists-but-uninspectable path — and the current failure evidence (last poll's stdout only) cannot discriminate the candidate mechanisms (unit flap/rate-limit, lost start, storage/query failure, clean accepted exit, unexcluded cross-test GC). Instrument first: capture the missing evidence on the next occurrence so the cause can be assigned instead of speculated.

## What Changes

- `live_identity` records capped escaped stdout AND stderr plus the exit/signal/timeout disposition on every poll outcome.
- `wait_landed_incarnation` additionally probes `podman container exists` per poll and retains first-anomaly time, outcome counts/transitions, and bounded first/last poll history; the bound panic reports all of it.
- On the timeout path only, one bounded post-mortem bundle is captured before Guard cleanup: `systemctl --user show` machine-readable unit properties, `podman ps -a` filtered to the test container, and a bounded unit journal slice. The original failure is retained even if a diagnostic query succeeds.
- Every diagnostic subprocess is bounded by the remaining poll budget; the per-inspect allowance is tightened so instrumentation cannot multiply the wait.
- No resets, restarts, relaunches, prunes, gc, locks, or serialization in the collector; unavailable/timeout evidence is marked as such.
- Backend-free pins for the new outcome classifications are added to `roster-check.py --self-test` alongside the existing fixtures.

## Capabilities

### New Capabilities

- `surgery-diagnostics`: test-only diagnostic evidence rules for the enter-gate surgery paths — what is recorded per poll, what the post-mortem bundle contains, subprocess bounds, and the no-mutation/no-lock collector constraints.

### Modified Capabilities

- None. No product behavior, gate semantics, refusal vocabulary, or roster/checker behavior changes. Test assertions are unchanged; only failure evidence is enriched.

## Impact

- `tests/integration/enter_gate.rs` only (helpers + the restart arm's wait/timeout path; the recreate arm shares the helpers).
- `roster-check.py --self-test` gains classification fixtures; no checker behavior change.
- No workflow, profile, roster, product source, or branch-protection changes. No new CI jobs or dispatches; evidence arrives via existing retained `suite.log` artifacts on the next natural occurrence.
