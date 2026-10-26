## Context

`wait_landed_incarnation` (`tests/integration/enter_gate.rs:254`) polls `live_identity` every 500ms against a 60s deadline and keeps only the last note; `live_identity` renders stdout only, discarding stderr and the `container exists` code. On timeout the bound panic reports the bound pair plus one stdout fragment — insufficient to separate unit-failed, start-lost, storage/query failure, clean accepted exit, or cross-test GC. The recreate arm shares the helpers. Stakeholders: Owner (diagnosis), tier-1 (packet constraints), tier-2 (consult verdict). Constraint: test-only, no product/gate/roster/workflow changes; observer perturbation minimized, bounded, and disclosed, never claimed absent.

## Goals / Non-Goals

**Goals:**
- Next occurrence of either signature carries evidence to support or eliminate hypotheses with stated limits; whatever cannot be separated stays explicitly unresolved.
- Every phase explicitly bounded under the stated end-to-end bound B+W+D+cleanup; the landing deadline W itself is unchanged.
- New classifications pinned through the actual Rust implementation (backend-free tests invoking the same functions), so the collector itself is tested.

**Non-Goals:**
- No product, gate, vocabulary, roster, workflow, or branch-protection changes.
- No retries, no absorb-125, no relaxed refusal vocabulary, no quarantine move.
- No new locks, serialization, resets, or cleanup in the collector.
- No change to the existing bound verdict wording: unqueryable outcomes are labeled unqueryable in the new report fields, and the current verdict text is left untouched.

## Decisions

- **Per-poll record (cheap commands only): inspect + `container exists`.** Both are single podman queries; `systemctl`/`ps`/journal stay out of the poll loop (no barrage). Rationale: exists-code 0/1/125 plus inspect stderr is the minimum that separates absent-at-that-instant from storage/query failure.
- **Bounded history, not full trace.** First-anomaly time, outcome counts/transitions, and first/last N poll records (N small, fixed). Rationale: 120 polls of full rendering would drown the log; transitions carry the flap signal.
- **Heavy post-mortem once, on the timeout path, before Guard cleanup.** `systemctl --user show` properties (LoadState/ActiveState/SubState/Result, ExecMainCode/Status, NRestarts, InvocationID, StartLimit settings, actual Restart/SuccessExitStatus, monotonic state-change times, main/control PIDs), filtered `podman ps -a` (name filter; known bound container ID compared against the listing in Rust, match recorded, absence of a match recorded), bounded journal slice from the recorded pre-surgery walltime/cursor. Rationale: snapshots and journal support hypotheses but cannot by themselves prove which actor removed a container; whatever the evidence cannot separate stays explicitly unresolved.
- **Bounded pre-surgery baseline adopted.** One inspect plus one exists outcome pair and one `systemctl show` snapshot before the surgery command, compared against at timeout. The bound pair is already known at bind time, so the marginal cost is two queries and one snapshot. The baseline sits outside the wait's budget (it runs before the wait starts) but inside its own small fixed baseline allowance B, so it cannot shift pre-surgery timing unboundedly; baseline failure or unavailability is recorded, never a new test-behavior gate. Alternative (timeout snapshot only) rejected: without a baseline, unit-failed vs clean-stop reads against assumption rather than a recorded state. A baseline that already shows an inactive unit or failed queries is an anomalous premise, not evidence the setup was healthy.
- **Phased budget B+W+D, not everything inside W.** Bounded pre-surgery baseline allowance B, unchanged 60s landing deadline W, separately bounded failure-report allowance D starting only after the original timeout latches, plus accounted kill/reap/drain cleanup; no new polling after W; exactly-once attempt with missing/unscheduled/timed-out diagnostics reported as such. Rationale: the post-mortem starts when no poll budget remains, so charging it to the remaining poll budget is incoherent; the stated end-to-end bound is B+W+D+cleanup. Each chosen value and its rationale are recorded in a code comment and in the implementation packet — not derived from retained suite.log artifacts, which carry no per-poll timings.
- **Pins execute the real Rust seam, not a Python model.** Record/classify/report/history functions live in a helper module beside the collector; backend-free tests invoke that same implementation. A duplicated Python taxonomy could stay green while the Rust path is wrong, so it cannot stand as product-path evidence; the quarantine checker stays scoped to quarantine. Rationale: executable connection over documented coupling.
- **Raw StartedAt kept intact end-to-end.** Compare full strings; never reconstruct from whitespace tokens. Rationale: podman 5.7 spaceful timestamps already caused one fail-open parse bug.
- **Original failure retained.** If a diagnostic query succeeds after the verdict failed, the log keeps the original failure first. Rationale: post-mortem success must not read as exoneration.

Alternatives considered: always-on journal/event observer (rejected — unbounded background cost and portability); prearmed lifecycle observer (rejected — unattributed GC-vs-unit/runtime removal stays explicit rather than instrumented away); reusing the audit job for diagnosis (rejected — wrong tool; evidence must arrive on the natural failing leg); asserting more in-test (rejected — verdicts frozen; instrumentation only).

## Risks / Trade-offs

- [Observer perturbation] Extra queries may stop reproduction, and the per-poll exists probe lands in the same wait the :357 exists-but-uninspectable observation traversed — so a disappearance of either class after instrumentation proves nothing about the race, and the two classes must not be conflated on that basis. Perturbation can only be minimized, bounded, and disclosed, never literally eliminated → Mitigation: record the stop as data, never as absence-of-race; keep :357 as its own class (landing-timeout scope; retained history dumped on later failures as context only); disclose in the packet.
- [Snapshot causal ambiguity] Unit state at timeout postdates the failure → Mitigation: monotonic timestamps, InvocationID, journal slice; state overlap explicitly rather than claiming order.
- [Log volume] History + post-mortem on every timeout → Mitigation: fixed caps; post-mortem only on the failure path.
- [Per-probe tightening changes timing] Smaller allowances could turn slow-inspect into timeout-marked → Mitigation: mark timeout evidence as such; the normative constraint is the total bound, and the fixed value plus rationale are recorded in code and packet.

## Migration Plan

Single packet: helpers + fixtures, backend-free self-test green, spec validation green, tier-1 then tier-2 review. No rollout steps; evidence arrives on the next natural CI occurrence. Rollback is revert.

## Open Questions

- Bounded history depth N (small; fixed at implementation).
