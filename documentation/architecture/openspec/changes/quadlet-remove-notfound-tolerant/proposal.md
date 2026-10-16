## Why

Concurrent `terminate` (CLI, lock-held) and attached `conduct` teardown (unlocked past guard drop) can both enter `remove_unit_file` and race on unlink: `exists()` → other-remover unlinks → `remove_file` ENOENT mapped to `Runtime` error. This matches issues/6's exact `remove ... .container: No such file` symptom and explains serial-suite failures without parallel tests.

## What Changes

- `remove_unit_file` treats filesystem `NotFound` on unlink as converged success (absent unit is the converged state, per existing doc intent), while all other IO errors still propagate.
- Successful unlink still performs `daemon-reload`; reload failure remains an error, never swallowed as absence.
- Deterministic filesystem removal-interleave pin (fails on baseline) + one live CLI-terminate/attached-conduct convergence boundary test.
- No lock changes; `stop_settle` silent-settle gap stays separate.

## Capabilities

### New Capabilities
- (none)

### Modified Capabilities
- `isolator-contract`: tighten idempotent-teardown requirement to cover concurrent-remover filesystem NotFound (converged success, reload preserved on actual unlink).

## Impact

- `src/isolators/quadlet.rs::remove_unit_file` only; no API, profile, or wire changes.
- Tests: one deterministic unit pin + one live boundary test; full-live gates unchanged.
