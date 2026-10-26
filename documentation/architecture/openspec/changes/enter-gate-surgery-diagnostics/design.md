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
- No change to the existing bound verdict wording: unqueryable outcomes are labeled unqueryable in the new report fields, and the current verdict text is left untouched.

## Decisions

- **Per-poll record (cheap commands only): inspect + `container exists`.** Both are single podman queries; `systemctl`/`ps`/journal stay out of the poll loop (no barrage). Rationale: exists-code 0/1/125 plus inspect stderr is the minimum that separates absent-at-that-instant from storage/query failure.
- **Bounded history, not full trace.** First-anomaly time, outcome counts/transitions, and first/last N poll records (N small, fixed). Rationale: 120 polls of full rendering would drown the log; transitions carry the flap signal.
- **Heavy post-mortem once, on the timeout path, before Guard cleanup.** `systemctl --user show` properties (LoadState/ActiveState/SubState/Result, ExecMainCode/Status, NRestarts, InvocationID, StartLimit settings), filtered `podman ps -a`, bounded journal slice. Rationale: snapshots can't prove causal order but unit-failed vs unit-active-clean-stop vs unit-gone discriminate hypotheses 1/2/4 from 3.
- **Bounded pre-surgery baseline adopted.** One inspect plus one exists outcome pair and one `systemctl show` snapshot before the surgery command, compared against at timeout. The bound pair is already known at bind time, so the marginal cost is two queries and one snapshot. The baseline sits outside the wait's budget (it runs before the wait starts) but inside its own small fixed allowance, so it cannot shift pre-surgery timing unboundedly. Alternative (timeout snapshot only) rejected: without a baseline, unit-failed vs clean-stop reads against assumption rather than a recorded state.
- **Tighten the per-probe allowance with the total bound as the normative constraint.** `live_identity` currently allows a fresh 60s per inspect inside a 60s outer window; every probe (inspect, exists, post-mortem) gets a small fixed allowance, and the total wait cannot exceed its bound under any per-probe value. The chosen value and its rationale are recorded in a code comment and in the implementation packet — not derived from retained suite.log artifacts, which carry no per-poll timings.
- **Backend-free pins in `roster-check.py --self-test`, coupling stated.** Classify synthetic poll/post-mortem fixtures: exists 0/1/125, inspect failure with stderr shapes, unqueryable labeling with no absence aggregation, signal/spawn/timeout dispositions, truncation markers, original-verdict retention against later-healthy observations. Same precedent as the quarantine gate fixtures: the collector's taxonomy is tested without cargo. This couples the gate tool's self-test to this test's internals — a classification change here means editing the gate tool's fixtures — accepted deliberately over an untested collector.
- **Raw StartedAt kept intact end-to-end.** Compare full strings; never reconstruct from whitespace tokens. Rationale: podman 5.7 spaceful timestamps already caused one fail-open parse bug.
- **Original failure retained.** If a diagnostic query succeeds after the verdict failed, the log keeps the original failure first. Rationale: post-mortem success must not read as exoneration.

Alternatives considered: always-on journal/event observer (rejected — unbounded background cost and portability); reusing the audit job for diagnosis (rejected — wrong tool; evidence must arrive on the natural failing leg); asserting more in-test (rejected — verdicts frozen; instrumentation only).

## Risks / Trade-offs

- [Observer perturbation] Extra queries may stop reproduction, and the per-poll exists probe lands in the same wait the :357 exists-but-uninspectable observation traversed — so a disappearance of either class after instrumentation proves nothing about the race, and the two classes must not be conflated on that basis → Mitigation: record the stop as data, never as absence-of-race; keep :357 as its own class; disclose in the packet.
- [Snapshot causal ambiguity] Unit state at timeout postdates the failure → Mitigation: monotonic timestamps, InvocationID, journal slice; state overlap explicitly rather than claiming order.
- [Log volume] History + post-mortem on every timeout → Mitigation: fixed caps; post-mortem only on the failure path.
- [Per-probe tightening changes timing] Smaller allowances could turn slow-inspect into timeout-marked → Mitigation: mark timeout evidence as such; the normative constraint is the total bound, and the fixed value plus rationale are recorded in code and packet.

## Migration Plan

Single packet: helpers + fixtures, backend-free self-test green, spec validation green, tier-1 then tier-2 review. No rollout steps; evidence arrives on the next natural CI occurrence. Rollback is revert.

## Open Questions

- Bounded history depth N (small; fixed at implementation).
