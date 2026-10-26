## Context

`wait_landed_incarnation` (`tests/integration/enter_gate.rs:254`) polls `live_identity` every 500ms against a 60s deadline and keeps only the last note; `live_identity` renders stdout only, discarding stderr and the `container exists` code. On timeout the bound panic reports the bound pair plus one stdout fragment — insufficient to separate unit-failed, start-lost, storage/query failure, clean accepted exit, or cross-test GC. The recreate arm shares the helpers. Stakeholders: Owner (diagnosis), tier-1 (packet constraints), tier-2 (consult verdict). Constraint: test-only, no product/gate/roster/workflow changes; observer must not perturb the race.

## Goals / Non-Goals

**Goals:**
- Next occurrence of either signature carries enough evidence to assign or eliminate each candidate mechanism.
- Every diagnostic subprocess provably bounded; total wait cannot grow.
- New classifications pinned backend-free so the collector itself is tested.

**Non-Goals:**
- No product, gate, vocabulary, roster, workflow, or branch-protection changes.
- No retries, no absorb-125, no relaxed refusal vocabulary, no quarantine move.
- No new locks, serialization, resets, or cleanup in the collector.

## Decisions

- **Per-poll record (cheap commands only): inspect + `container exists`.** Both are single podman queries; `systemctl`/`ps`/journal stay out of the poll loop (no barrage). Rationale: exists-code 0/1/125 plus inspect stderr is the minimum that separates absent-at-that-instant from storage/query failure.
- **Bounded history, not full trace.** First-anomaly time, outcome counts/transitions, and first/last N poll records (N small, fixed). Rationale: 120 polls of full rendering would drown the log; transitions carry the flap signal.
- **Heavy post-mortem once, on the timeout path, before Guard cleanup.** `systemctl --user show` properties (LoadState/ActiveState/SubState/Result, ExecMainCode/Status, NRestarts, InvocationID, StartLimit settings), filtered `podman ps -a`, bounded journal slice. Rationale: snapshots can't prove causal order but unit-failed vs unit-active-clean-stop vs unit-gone discriminate hypotheses 1/2/4 from 3.
- **Tighten the per-probe allowance and charge it to the remaining budget.** `live_identity` currently allows a fresh 60s per inspect inside a 60s outer window; every probe (inspect, exists, post-mortem) gets a small fixed allowance (seconds, not tens) deducted from the remaining deadline. Rationale: tier-2 constraint; instrumentation must not multiply the wait.
- **Raw StartedAt kept intact end-to-end.** Compare full strings; never reconstruct from whitespace tokens. Rationale: podman 5.7 spaceful timestamps already caused one fail-open parse bug.
- **Original failure retained.** If a diagnostic query succeeds after the verdict failed, the log keeps the original failure first. Rationale: post-mortem success must not read as exoneration.
- **Backend-free pins in `roster-check.py --self-test`.** Classify synthetic poll/post-mortem fixtures (exists 0/1/125, inspect-failure+stderr shapes, timeout markers, truncation). Rationale: existing precedent; the collector's taxonomy is tested without cargo.

Alternatives considered: always-on journal/event observer (rejected — unbounded background cost and portability); reusing the audit job for diagnosis (rejected — wrong tool; evidence must arrive on the natural failing leg); asserting more in-test (rejected — verdicts frozen; instrumentation only).

## Risks / Trade-offs

- [Observer perturbation] Extra queries may stop reproduction → Mitigation: record the stop as data, never as absence-of-race; disclose in the packet.
- [Snapshot causal ambiguity] Unit state at timeout postdates the failure → Mitigation: monotonic timestamps, InvocationID, journal slice; state overlap explicitly rather than claiming order.
- [Log volume] History + post-mortem on every timeout → Mitigation: fixed caps; post-mortem only on the failure path.
- [Per-probe tightening changes timing] Smaller allowances could turn slow-inspect into timeout-marked → Mitigation: mark timeout evidence as such; allowance chosen above observed p99 of successful polls.

## Migration Plan

Single packet: helpers + fixtures, backend-free self-test green, spec validation green, tier-1 then tier-2 review. No rollout steps; evidence arrives on the next natural CI occurrence. Rollback is revert.

## Open Questions

- Exact per-probe allowance values (to be set from observed successful-poll timings at implementation).
- Bounded history depth N (small; fixed at implementation).
