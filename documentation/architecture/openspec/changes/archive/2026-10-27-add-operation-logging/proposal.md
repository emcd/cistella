## Why

The first instrumented honest-red capture (post-restart query failure with the unit gone by post-mortem) could not name the actor that removed the unit file: concurrent global `gc` sweeps overlapped the failure window, but overlap timing is not a mutation receipt. Cistella keeps no durable operation log, so cross-session attribution after the fact is impossible. Structured logging, starting with but not limited to the gc actor, closes that gap.

## What Changes

- Add durable, bounded, per-operation logging to Cistella: one operation log per run/operation id, with per-session and per-attempt records inside.
- Log the gc actor's per-target plan, classification, action, and result (not only a final reaped list), plus write-failure and partial-record handling that never changes gc decisions.
- Generalized logging support plus gc logging as its first consumer, in one change by explicit operator scope expansion: cover the gc actor first, then conduct install/start, terminate, and shared teardown in sequence (what ran, what it observed, what it decided). Reviewed as the larger design, not the small-gc vehicle.
- Anchor logs under XDG_STATE_HOME with the repo's existing secure-path discipline; bound size, rotation, and retention; keep raw logs separate from sanitized excerpts.
- Explicitly not changing: gc scan/reap behavior, ownership, eligibility, fail-closed semantics, or any product decision path. Logging is observability only.
- Named dependency outside this product change: on hosted CI runners the state directory dies with the job, so the motivating use case (attributing a CI red after the fact) additionally requires uploading the operation logs as a runner artifact. That upload is a workflow change, not part of this change; without it the durable-attribution claim holds for the local seat only.

## Capabilities

### New Capabilities

- `operation-logging`: durable per-operation logs (layout, anchoring, identity metadata, bounded retention, raw/sanitized separation, failure semantics for the logger itself).

### Modified Capabilities

- None: no product decision path changes. Gc and lifecycle requirement wording is untouched; the log records decisions without influencing them.

## Impact

- New log-writing code on four lifecycle paths (gc first, then conduct install/start, terminate, teardown call sites); new tests pinning log shape, the never-influences-decisions property, and the crash/torn/overflow reader paths on the real seam.
- New on-disk state under XDG_STATE_HOME (fallback `~/.local/state`) bounded by a 3 GiB total tree budget with a protected 1 GiB failure reserve; bounded disclosed observer perturbation (append-only, fsync on close, prune off the hot path), never zero effect by claim.
- Test diagnostics (e.g. the surgery collector) can correlate against operation logs instead of inferring actors from timing.
