## 1. Product surface (todos/49)

- [x] 1.1 Owner pre-captures incarnation before `execute_launch`, post-checks after Ok, publishes `cistella-attestation-<id>` (0600, atomic temp+linkat with EEXIST-preserving write-once, anchored-discipline I/O) ONLY on equality; mismatch/unknown refuses + converges via typed teardown; teardown removes unit file before attestation reap; gc reaps orphans
- [x] 1.2 `describe state` verb: commitment + snapshot where proven (never `executing`); backend=unknown without provenance (no snapshot); residue table with scope marking; fuzzy Selector errors unchanged; foreign unsupported/no-record rules; default `inspect` byte-identical
- [x] 1.3 `enter` single-pass check (attestation + incarnation + native admittability), immediate typed refusal; stale-generation refusal; post-check exec failure honest; foreign legacy direct path; no retries, no polls
- [x] 1.4 Repair `inspect()` executing scope (`pending.unit == *handle` via shared predicate + inline scope pin); pre-commit refusal via the enter-gate negative control + commit-decision no-publication pins
- [x] 1.5 Pins: absent/state-success vs enter-refusal split (stalled pair), corrupt/mismatch unit pins, stale restart+recreate live pins, post-teardown Selector ordering, residue rows + typo-id + attestation-only live probes, foreign gating (shape + unsupported probe), pre-change unattested window (stalled pair), same-name recreate/restart refusal, choreographed launch-interval refusal (commit_decision injected pins)

## 2. Race fixes (issues/11, issues/12)

- [x] 2.1 Convert `lifecycle::conduct_lifecycle_terminate_while_attached` to the commit-event edge: arm inotify watch → recheck existing/matching attestation → validate contents + incarnation after wake (fail-loud bound); pre-commit enter refuses typed (issues/12)
- [x] 2.2 Unify post-terminal dispatcher sends into one documented typed error (send-refusal + orphaned-disconnect paths); pin the unified string; first-terminal := fatal-exchange terminal only
- [x] 2.3 wire_death admitted case: peer mode reads N ops before the lingering-descendant exit, host sends N back-to-back with no sleeps, all name the shutdown failure with uncertain-set and no death latch (issues/11)
- [x] 2.4 wire_death post-terminal case: op after the first fatal terminal gets exactly the unified error; `death_checked` dominance asserted; FD/rendezvous holders kept to post-observation explicit scope-drop release, no timers

## 3. Handshake conversions (artifacts/7)

- [x] 3.1 signals.rs start-delay/lock-attempt → commit-event-anchored waits with fail-loud backstops
- [x] 3.2 wire_death FD-holder → readiness/release handshake + injected populated-pending tests
- [x] 3.3 tmpdir natural-exit observation → commit-event-anchored wait with fail-loud backstop
- [x] 3.4 Retained boundary + negative control per conversion (Advisor confidence model): sighup keeps 129/teardown/no-residue asserts, gate-negative via the shared pre-commit refusal pin; wire_death keeps converge+close reporting with post-terminal+dominance as the complementary case; tmpdir keeps mid-session slice + litter-free asserts, gate-negative via the shared pre-commit refusal pin; await_observed helper pins arm→recheck→validate + fail-loud bound directly (event_wait.rs)

## 4. Ride-along (todos/50)

- [x] 4.1 Forced reload-failure exercise for `remove_unit_file` (live fault-injection seat, shimmed systemctl degrading only daemon-reload; recovery converges)

## 5. Validation

- [x] 5.1 `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `openspec validate --all --strict`, `git diff --check` (all green seat-side)
- [x] 5.2 Full live tier on QA seat (816/816 green at 4ee7097, 22/22 named paths PASS, empty skip roster for required paths, attributed residue deltas, rdv backlog swept; receipt: cistella:artifacts/8). 26.04-leg CI watch on issues/11, issues/12 is POST-PUSH observation (branch unpushed; no CI leg has exercised this code yet).
