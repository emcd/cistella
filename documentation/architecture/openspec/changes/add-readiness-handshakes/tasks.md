## 1. Product surface (todos/49)

- [x] 1.1 Owner pre-captures incarnation before `execute_launch`, post-checks after Ok, publishes `cistella-attestation-<id>` (0600, O_EXCL, atomic temp+rename, anchored-discipline I/O) ONLY on equality; mismatch/unknown refuses + converges via typed teardown; teardown removes unit file before attestation reap; gc reaps orphans
- [x] 1.2 Dedicated `state` verb: commitment + snapshot (never `executing`); residue table (live-residue error / attestation-only Absent / all-absent / probe-error); fuzzy Selector errors unchanged; foreign unavailable semantics; default `inspect` byte-identical
- [x] 1.3 `enter` single-pass check (attestation + incarnation + native admittability), immediate typed refusal; stale-generation refusal; post-check exec failure honest; foreign legacy direct path; no retries, no polls
- [ ] 1.4 Repair `inspect()` executing scope (`pending.unit == *handle`); pins: other-unit-executing and pre-commit-child-is-not-ack
- [ ] 1.5 Pins: absent/state-success vs enter-refusal split, corrupt/mismatch/stale/deletion-race, residue rows, typo-id truthfulness, foreign gating + foreign no-record probe unsupported, pre-change unattested window, same-name recreate/restart refusal, choreographed launch-interval recreate refusal (injected, deterministic)

## 2. Race fixes (issues/11, issues/12)

- [x] 2.1 Convert `lifecycle::conduct_lifecycle_terminate_while_attached` to the commit-event edge: arm inotify watch → recheck existing/matching attestation → validate contents + incarnation after wake (fail-loud bound); pre-commit enter refuses typed (issues/12)
- [x] 2.2 Unify post-terminal dispatcher sends into one documented typed error (send-refusal + orphaned-disconnect paths); pin the unified string; first-terminal := fatal-exchange terminal only
- [x] 2.3 wire_death admitted case: peer mode reads N ops before the lingering-descendant exit, host sends N back-to-back with no sleeps, all name the shutdown failure with uncertain-set and no death latch (issues/11)
- [x] 2.4 wire_death post-terminal case: op after the first fatal terminal gets exactly the unified error; `death_checked` dominance asserted; FD/rendezvous holders kept to post-observation explicit scope-drop release, no timers

## 3. Handshake conversions (artifacts/7)

- [x] 3.1 signals.rs start-delay/lock-attempt → commit-event-anchored waits with fail-loud backstops
- [x] 3.2 wire_death FD-holder → readiness/release handshake + injected populated-pending tests
- [x] 3.3 tmpdir natural-exit observation → commit-event-anchored wait with fail-loud backstop
- [ ] 3.4 Retained boundary + negative control per conversion (Advisor confidence model)

## 4. Ride-along (todos/50)

- [ ] 4.1 Forced reload-failure exercise for `remove_unit_file` (live fault-injection seat, shimmed systemctl)

## 5. Validation

- [ ] 5.1 `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `openspec validate --all --strict`, `git diff --check`
- [ ] 5.2 Fast suite + full live tier on QA seat; 26.04-leg CI watch on issues/11, issues/12
