## Why

The 0.4 audit (Advisor reviews/12, QA pattern book, owner
inventory) found the live suite's confidence resting partly on
harness behaviors that are not themselves proven: unbounded calls
behind nominal deadlines, cross-test gc interference, shared
namespaces without ownership, and evidence lost before cleanup.
Timer-based tests sit on top of this substrate — hardening it
first gives every later conversion stable ground. Four source
files additionally sit in the 990s warn zone and have already
vetoed one commit; splitting them now prevents review friction
later.

## What Changes

- Bound every previously-unbounded harness call (conduct id-line
  read, `run_cistella` output, `wait_active`/`wait_harness`
  subprocess polls, PTY read/waitpid) with per-call timeouts;
  nominal loop deadlines become hard wall bounds.
- Scope Guard-Drop `gc` so one fixture's cleanup cannot reap
  another's stopped sessions; hold orphan protection across
  inspect (close the unlock-then-inspect window).
- Audit all shared-namespace assertions (`/tmp`, env, registry)
  to caller-owned names or serialization (registry sess-N fix as
  the pattern); tag tests by required seat capability and log
  every environment-gate skip.
- Apply the slice-litter rule (todos/51): per-run unique names
  or explicit post-assert cleanup for retaining unit-test
  artifacts.
- Extend original-stream diagnostics to remaining bare asserts;
  fix `wait_harness` binding-inference comment; reconcile the
  sighup test name with its actual signal.
- Split `main`/`client`/`protocol`/`podman` past the warn zone
  along existing seams (no behavior change; line-count hook
  stays green with margin).

## Capabilities

### New Capabilities
- (none)

### Modified Capabilities
- (none — harness-only and structural; no spec behavior changes)

## Impact

- `tests/integration/helpers.rs` (+ bounded-call discipline),
  `tests/integration/orphan.rs`, `selectors.rs`,
  `tests/unit/registry.rs` (audit pattern), capability/skip
  logging across live files, `src/main.rs`,
  `src/isolators/{client,podman}.rs`, `src/framework/protocol.rs`
  (splits only).
- Reviews: Tier-1 → Tier-2; QA live loop (interference-sensitive
  tests re-run).
