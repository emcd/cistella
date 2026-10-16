## 1. Bounded harness calls

- [ ] 1.1 Bound the conduct id-line read (nonblocking/short-deadline reads, silent-child cutoff + pin).
- [ ] 1.2 Bound `run_cistella` subprocess waits (per-call timeout, kill + drain on expiry + pin).
- [ ] 1.3 Bound `wait_active`/`wait_harness` poll invocations + PTY read/waitpid windows (compose with remaining outer deadline, never fresh budgets; stuck-poll + flood/held-writer pins).
- [ ] 1.4 Correct the `wait_harness` binding-inference comment to session-up smoke.

## 2. Interference-free fixtures

- [ ] 2.1 Scope Guard-Drop gc to owned sessions (minted IDs + owned records only; never globally destructive behind a lock).
- [ ] 2.2 Hold orphan protection across inspect (close the unlock-then-inspect window).
- [ ] 2.3 Reconcile the sighup test name with its actual signal (rename or send HUP).

## 3. Ownership, tags, litter, diagnostics

- [ ] 3.1 Audit shared-namespace assertions file by file (sess-N pattern); serialize or scope; unique names must survive concurrent suite invocations.
- [ ] 3.2 Tag tests by seat capability; log every environment-gate skip; report executed vs soft-skipped distinctly.
- [ ] 3.3 Apply the slice-litter rule (todos/51) to retaining unit fixtures.
- [ ] 3.4 Original-stream diagnostics on remaining bare asserts.

## 4. Slimming splits (moves only)

- [ ] 4.1 Split `main.rs` along verb seams, green per split.
- [ ] 4.2 Split `client.rs`, `protocol.rs`, `podman.rs` past warn-zone margins, green per split.

## 5. Review and sync

- [ ] 5.1 Tier-1 review → Tier-2 review.
- [ ] 5.2 QA loop focused on interference-sensitive tests.
