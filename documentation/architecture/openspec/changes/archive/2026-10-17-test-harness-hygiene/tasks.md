## 1. Bounded harness calls

- [x] 1.1 Bound the conduct id-line read (nonblocking/short-deadline reads, silent-child cutoff + pin).
- [x] 1.2 Bound `run_cistella` subprocess waits (per-call timeout, kill + drain on expiry + pin).
- [x] 1.3 Bound `wait_active`/`wait_harness` poll invocations + PTY read/waitpid windows (compose with remaining outer deadline, never fresh budgets; stuck-poll + flood/held-writer pins).
- [x] 1.4 Correct the `wait_harness` binding-inference comment to session-up smoke.
- [x] 1.5 Migrate test-body `.output()`/`.wait()` call sites onto `run_bounded`/`reap_bounded` (same-pattern mechanical sweep, own commit).

## 2. Interference-free fixtures

- [x] 2.1 Scope Guard-Drop gc to owned sessions (minted IDs + owned records only; never globally destructive behind a lock).
- [x] 2.2 Hold orphan protection across inspect (close the unlock-then-inspect window).
- [x] 2.3 Reconcile the sighup test name with its actual signal (rename or send HUP).

## 3. Ownership, tags, litter, diagnostics

- [x] 3.1 Audit shared-namespace assertions file by file (sess-N pattern); serialize or scope; unique names must survive concurrent suite invocations.
- [x] 3.2 Tag tests by seat capability; log every environment-gate skip; report executed vs soft-skipped distinctly.
- [x] 3.3 Apply the slice-litter rule (todos/51) to retaining unit fixtures.
- [x] 3.4 Original-stream diagnostics on remaining bare asserts.

## 4. Slimming splits (moves only)

- [x] 4.1 Split `main.rs` along verb seams, green per split.
- [x] 4.2 Split `client.rs`, `protocol.rs`, `podman.rs` past warn-zone margins, green per split.

## 5. Review and sync

- [x] 5.1 Tier-1 review → Tier-2 review.
- [x] 5.2 QA loop focused on interference-sensitive tests.
