## 1. Poll record and history

- [x] 1.1 Extend `live_identity` to return per-command start/end monotonic offsets and query order, capped escaped stdout AND stderr verbatim (no prose diagnosis), plus exit/signal/spawn/timeout disposition; keep raw StartedAt intact
- [x] 1.2 Add per-poll `podman container exists` probe under the same per-probe allowance; record its 0/1/125 code next to the inspect outcome
- [x] 1.3 Retain first-anomaly time, outcome counts/transitions, and bounded first/last N poll records in `wait_landed_incarnation`; report all in the bound panic
- [x] 1.4 Capture the bounded pre-surgery baseline under allowance B (bound pair, one inspect+exists outcome pair, one `systemctl --user show` snapshot) before the surgery command; record baseline failure/unavailability without new behavior gates

## 2. Phased budget and timeout post-mortem

- [x] 2.1 Implement explicit phases (baseline B, landing W unchanged at 60s, failure-report D after latching the timeout, accounted cleanup); no new polling after W; report missing/unscheduled/timed-out diagnostics as such
- [x] 2.2 Capture the heavy post-mortem once on the timeout path inside D before Guard cleanup (unit properties, filtered `podman ps -a`, journal slice from the pre-surgery cursor)
- [x] 2.3 Lead with the original failure directly adjacent to the prominently labeled unqueryable note; append later-healthy observations as subsequent state only
- [x] 2.4 Dump the retained bounded landing history and baseline alongside any later post-landing failure as context only, verdict semantics unchanged

## 3. Collector constraints

- [x] 3.1 Verify the collector issues no reset/restart/relaunch/prune/gc/lock/serialization commands (grep audit plus review); record perturbation minimization/bound/disclosure
- [x] 3.2 Measure and log the ownerless-setup kill/reap results instead of assuming owner death (observation only, no helper behavior change)

## 4. Real-seam pins and validation

- [x] 4.1 Put record/classify/report/history/budget functions in a helper module beside the collector with backend-free `#[cfg(test)]` pins invoking the same implementation (exists codes, stderr shapes, unqueryable labeling with no absence aggregation, truncation/timeout markers, original-verdict retention, history caps, budget composition); no `roster-check.py` changes
- [x] 4.2 `cargo fmt`, `clippy`, `openspec validate --all --strict` clean; backend-free pins green; `git diff --check` clean
- [x] 4.3 Route the packet through tier-1, then tier-2 (diagnostics only; no verdict, roster, workflow, or product changes)
