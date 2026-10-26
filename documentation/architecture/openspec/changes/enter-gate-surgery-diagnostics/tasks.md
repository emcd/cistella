## 1. Poll record and history

- [ ] 1.1 Extend `live_identity` to return capped escaped stdout AND stderr plus exit/signal/timeout disposition; keep raw StartedAt intact
- [ ] 1.2 Add per-poll `podman container exists` probe with the same bounded allowance; record its 0/1/125 code next to the inspect outcome
- [ ] 1.3 Retain first-anomaly time, outcome counts/transitions, and bounded first/last N poll records in `wait_landed_incarnation`; report all in the bound panic

## 2. Timeout post-mortem

- [ ] 2.1 Capture once on the timeout path before Guard cleanup: `systemctl --user show` properties, filtered `podman ps -a`, bounded journal slice
- [ ] 2.2 Charge every diagnostic subprocess to the remaining poll budget; tighten the per-probe allowance; mark timeout evidence as timeout
- [ ] 2.3 Lead the report with the original failure; append later-healthy observations as subsequent state only

## 3. Collector constraints

- [ ] 3.1 Verify the collector issues no reset/restart/relaunch/prune/gc/lock/serialization commands (grep audit plus review)
- [ ] 3.2 Measure the ownerless-setup kill/reap results and log the owner-dead premise instead of assuming it (observation only, no helper behavior change)

## 4. Pins and validation

- [ ] 4.1 Add backend-free taxonomy fixtures to `roster-check.py --self-test` (exists codes, stderr shapes, truncation/timeout markers)
- [ ] 4.2 `cargo fmt`, `clippy`, `openspec validate --all --strict` clean; self-test green; `git diff --check` clean
- [ ] 4.3 Route the packet through tier-1, then tier-2 (diagnostics only; no verdict, roster, workflow, or product changes)
