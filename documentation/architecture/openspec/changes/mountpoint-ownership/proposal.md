## Why

Any session whose bind-mount target has auto-created parents is broken for sibling creation: podman makes missing container-target parents root:root 755, seat processes run as container uid 1000 (keep-id), and `mkdir` under those parents fails `Permission denied` — reproduced live, byte-identical to the agentmux `.config/agentmux` failure (`cistella:todos/18`). Per-directory triples work around it case by case; every new config dir rediscovers the bug. Fix at the driver level now, while the mechanism is freshly proven.

## What Changes

- After `start_quadlet` succeeds and before the harness attaches, `conduct` prepares mountpoint parents: for each bind triple target, walk ancestors from the target's parent up to (excluding) `/`, skipping any ancestor that is itself a mountpoint (triple targets, tmpfs home, session-home) — `mkdir -p` plus conditional chown to the seat uid for root-owned dirs only, via `podman exec --user root` (explicit, never the observed-but-unpinned exec default).
- Seat uid is read live from `podman exec <ctr> id -u`, not assumed from keep-id.
- The step runs inside the creation window and fails closed through the existing teardown path (any preparation failure tears down, typed error).
- Mount roots are never touched (chowning through a bind mount would reach host dirs); `/` is never touched; RO targets keep RO (only their non-mounted parents gain traversal/creation rights).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `runtime`: the conduct lifecycle gains a mountpoint-preparation step with the mountpoint-exclusion and fail-closed rules above.

## Impact

- `conduct_session` in `src/main.rs` plus a small `runtime.rs` helper (pure ancestor computation, unit-testable) and exec wiring.
- One live test: deep-target session where the harness creates a sibling dir under auto-created parents (the exact reproduction), plus a unit test for the ancestor computation (exclusions, `/` boundary).
- No profile schema, transport, registry, or CLI changes. The unpinned-exec-default observation becomes a transport follow-up, not this change.
