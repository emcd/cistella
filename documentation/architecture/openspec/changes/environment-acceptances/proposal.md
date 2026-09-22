## Why

Containerized seats lose caller-provided process environment at the `cistella conduct` boundary: profiles can only resolve `core:` / `supplement:` / `environment:`(HOME-only) template spans, with no way to forward invoker-owned variables. This broke in-container Agentmux MCP discovery fleet-wide on the 0.1.0 rollout (relay-set `AGENTMUX_*` identity vars never reach the seat). A first-class acceptance mechanism is the 0.1.1 showstopper fix.

## What Changes

- New top-level profile key `environment-acceptances = [...]`: exact variable names (no globs for 0.1.1) accepted verbatim from the invoker environment into container env.
- **BREAKING**: `[environment]` table renamed to `[environment-assignments]`, symmetric with acceptances (assignments are profile-author values resolved through templates; acceptances are invoker-controlled values passed verbatim).
- Every listed acceptance is required: absent at conduct time fails before any session/runtime mutation — no unit file, scratch directory, or container (XDG profile seeding during named lookup precedes parsing and is outside this boundary) — with name-only (value-free) diagnostics.
- Any destination-name collision is a conduct-time error: assignment-vs-acceptance overlap and acceptance-vs-driver-owned/implicit names (`HOME`, `SSH_AUTH_SOCK` when credential-surface supplies it). No silent shadowing in either direction.
- Accepted values pass through verbatim (no template substitution applied to their values) but through the existing environment-value safety gate: non-Unicode and line-breaking values rejected value-free; ordinary `=` permitted. Names validated against the env-name grammar; duplicates rejected; deterministic first-error order over profile-list order.
- Acceptance grants conveyance only, never render permission: `{{environment:<accepted>}}` spans elsewhere do not resolve (acceptance confers no allowlist membership). A future `environment-acceptances-renderable`-shaped opt-in can revisit this; 0.1.1 does not include it (reserved direction tracked in `todos/profiles/12`).
- Explicit operator decision: **no blacklist, allowlist, or credential-deny applies to `environment-acceptances`**. An operator naming a variable — including a secret-shaped one — does so consciously; second-guessing is refused UX. Secrets belong in passthrough, never in templates.
- Template allow/deny policy (`{{environment:*}}` rules) is unchanged in 0.1.1; its redesign moves to 0.2.0.

## Capabilities

### New Capabilities

- `environment-acceptances`: caller-environment forwarding into container env (declaration, requiredness, collision errors, verbatim value safety, value-free diagnostics, no-deny rule).

### Modified Capabilities

- `mounts`: profile schema gains the top-level `environment-acceptances` key and renames `[environment]` to `[environment-assignments]` (requirement-level schema change; resolution/validation behavior for assignments unchanged).

## Impact

- `src/profile.rs` (schema, validation), conduct resolution path, unit rendering (accepted env into container env).
- Profiles: two host seats, baked example(s), docs (`documentation/usage/profiles.md`), trust-model note (accepted secrets rest transiently in generated unit files for the session lifetime — conveyed, never displayed).
- Tests: unit matrix (required/collision/verbatim/grammar/dupes/order) plus live conduct proving relay-var forwarding end to end.
