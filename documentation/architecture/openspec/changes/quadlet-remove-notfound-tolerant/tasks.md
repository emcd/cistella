## 1. Deterministic pin (fails on baseline)

- [x] 1.1 Add filesystem removal-interleave pin reproducing exists → peer-removes → unlink ENOENT against `remove_unit_file` logic; verify it fails on `baa7b98`. (Pins green; baseline check: new-seam tests fail compilation on stashed baseline. Honest negative control added per Tier-2: `baseline_error_mapping_fails_on_peer_removal` exhibits the old arm's ENOENT failure on identical facts — old-errors/new-converges pair is the regression proof, not compilation failure.)
- [x] 1.2 Non-NotFound IO errors propagate (directory + permission-denied unit pins). Production reload-attempt/reload-failure wiring is systemd-gated and CANNOT be pinned deterministically here — open follow-up tracked actionably as `cistella:todos/50` (forced reload-failure exercise), not closed by this repair.

## 2. Narrow repair

- [x] 2.1 Replace `exists()`-guarded removal in `remove_unit_file` with direct `remove_file`: `Ok` → reload, `ErrorKind::NotFound` → converged `Ok` without reload; all other errors and reload failure still propagate.
- [x] 2.2 Re-run deterministic pin green; run fast suite both umasks + fmt/clippy/validate/diff-check.

## 3. Live boundary proof + diagnostics

- [x] 3.1 Live CLI-terminate/attached-conduct boundary with readiness/interleave control (no new sleeps, no creation-lock across the concurrent window); capture both dispositions and assert final unit/service/container absence with cleanup ordered after assertions. Readiness is session-up facts (service-active + harness-visible) with honest non-ack framing — no host surface reads the guest table (filed as 0.4 todos/49). Absence asserts only documented codes (container-exists 1, explicit service state). (Implemented; compiles + gates; LIVE EXECUTION belongs to 3.3 QA loop.)
- [x] 3.2 Second-terminate diagnostics in `labels.rs` (original status+stdout+stderr emitted + panicked, no optional post-mortem — Tier-2 preferred omission over an unsound bounded runner) plus a bounded retained buffer on the conduct drain path (`ConductTaps`/`spawn_conduct_tapped`; `finalize` joins ONLY after drain completion, timeout returns labeled partial; pinned by deterministic held-open/completion cases in `conduct_taps.rs`); retain both stderrs and drain pipes throughout.
- [ ] 3.3 QA live loop on host (fast 639 + live incl. new boundary test + labels 4/4); correlate any recurrence against CI second-terminate evidence shape.

## 4. Review and sync

- [ ] 4.1 Tier-1 review → Tier-2 review of repair + tests.
- [ ] 4.2 Sync `isolator-contract` delta to main spec + changelog; keep `stop_settle` settle-loop gap as separate follow-up.
