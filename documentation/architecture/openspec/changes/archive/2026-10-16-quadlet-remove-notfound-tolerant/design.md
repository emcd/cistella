## Context

`remove_unit_file` (`src/isolators/quadlet.rs:510-534`) guards `remove_file` with `path.exists()`, then maps every IO error to `Runtime` — including `NotFound` when a concurrent remover (CLI `terminate` under the creation lock vs attached `conduct` past guard drop at `main.rs:805`, unlocked teardown at `:917`) unlinks first. The function's own doc (`:504-505`) and `isolator-contract` idempotent-teardown already promise absence-tolerance; implementation lags contract. Baseline `baa7b98`.

## Goals / Non-Goals

- Goals: make concurrent-remover NotFound converge successfully; preserve reload-on-unlink and reload-failure-is-error; pin with deterministic interleave test + one live boundary test.
- Non-Goals: no lock-scheme changes; no `stop_settle` settle-loop changes (separate gap); no swallowing of non-NotFound IO errors; no tombstone/eviction semantics changes.

## Decisions

- Map `ErrorKind::NotFound` from `remove_file` to `Ok` (skip reload — this caller made no filesystem change; the unlink winner owns reload/error reporting) over `remove-then-ignore-all-errors`: preserves failure signal for permissions/IO faults. NotFound proves nothing about the peer's reload — the peer may be paused pre-reload or its reload may fail; loser absence semantics are explicit, not global-convergence claims.
- Call `remove_file` directly without the `exists()` pre-check: `Ok` → reload, `NotFound` → converged `Ok` without reload. Identical reload behavior with one fewer syscall and no check race; `exists()` additionally risks suppressing stat/permission errors as false absence, which direct unlink surfaces normally.
- Live boundary test uses readiness/interleave control (both dispositions captured, final absence asserted post-teardown, cleanup assertions ordered after) rather than sleeps; creation lock must not be held across the concurrent window or the race under test is excluded.

## Risks / Trade-offs

- [NotFound masks a real deletion failure] → Mitigation: only `ErrorKind::NotFound` is tolerated; reload failure after successful unlink still errors; acceptance (3) pins this.
- [Live boundary test itself flakes] → Mitigation: readiness handshake, bounded waits, both stderrs retained, pipes drained; per Advisor acceptance (4).
- [Correlation to every historical issues/6 failure unproven] → Mitigation: treated as contract-alignment bugfix (defect established at source); closure records source-defect + deterministic/live evidence separately from the residual issues/6 watch. The unprinted historical stderr is unavailable and will not be retrieved — no closure depends on it.

## Migration Plan

Single-commit repair + two tests through Tier-1 → Tier-2 review; no deployment steps (behavior only converges where it previously errored).

## Open Questions

- None blocking; issues/6 stays on residual watch for additional mechanisms regardless of this repair's closure.
