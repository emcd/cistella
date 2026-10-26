## Why

The `conduct_enter_refuses_stale_incarnation_after_restart` live test fails intermittently in CI with two observed signatures — sustained `podman inspect` exit 125 across the 60s landing window, and a fast post-restart gate error from the exists-but-uninspectable path — and the current failure evidence (last poll's stdout only) cannot discriminate the candidate mechanisms (unit flap/rate-limit, lost start, storage/query failure, clean accepted exit, unexcluded cross-test GC). Instrument first: capture the missing evidence on the next occurrence so hypotheses can be supported or eliminated with stated limits, and whatever cannot be separated stays explicitly unresolved.

## What Changes

- `live_identity` records per-command start/end offsets and query order, capped escaped stdout AND stderr, plus the exit/signal/spawn/timeout disposition on every poll outcome; stderr is retained verbatim with no prose diagnosis.
- `wait_landed_incarnation` additionally probes `podman container exists` per poll and retains first-anomaly time, outcome counts/transitions, and bounded first/last poll history; the bound panic reports all of it.
- On the timeout path only, one bounded post-mortem bundle is captured inside a separately bounded failure-report allowance after the original timeout latches: `systemctl --user show` machine-readable unit properties, `podman ps -a` filtered to the test container name with the known bound container ID compared against the listing in Rust (match recorded; absence of a match recorded as such), and a bounded unit journal slice from the recorded pre-surgery walltime/cursor. The original failure leads, directly adjacent to a prominently labeled unqueryable note.
- Explicit phases with a stated end-to-end bound (baseline B, landing W unchanged at 60s, failure-report D, plus accounted cleanup); no new polling after W; missing/unscheduled/timed-out diagnostics reported as such.
- The bounded landing history and pre-surgery baseline are dumped alongside any later post-landing failure as context only; the :357 exists-but-uninspectable class keeps its own verdict semantics, and no prearmed lifecycle observer is added.
- No resets, restarts, relaunches, prunes, gc, locks, or serialization in the collector; unavailable/timeout evidence is marked as such. Perturbation is minimized, bounded, and disclosed, never claimed absent.
- Backend-free pins invoke the actual Rust collector functions through a backend-free test module beside the collector (same implementation, no Python model); the quarantine checker stays scoped to quarantine.

## Capabilities

### New Capabilities

- `surgery-diagnostics`: test-only diagnostic evidence rules for the enter-gate surgery paths — what is recorded per poll, what the post-mortem bundle contains, subprocess bounds, and the no-mutation/no-lock collector constraints.

### Modified Capabilities

- None. No product behavior, gate semantics, refusal vocabulary, or roster/checker behavior changes. Test assertions are unchanged; only failure evidence is enriched.

## Impact

- `tests/integration/enter_gate.rs` (helpers plus the restart arm's wait/timeout path; the recreate arm shares the helpers) and a backend-free test module beside the collector following the existing `blocking.rs` pattern (`#[cfg(test)]` pins calling the real predicates through the same implementation, no new target, no duplicated model).
- No `roster-check.py` changes; the quarantine checker stays scoped to quarantine.
- No workflow, profile, roster, product source, or branch-protection changes. No new CI jobs or dispatches; evidence arrives via existing retained `suite.log` artifacts on the next natural occurrence.
