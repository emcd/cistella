## Why

`profile-templates` shipped expansion for mount triples and command argv only: `template_values()` never scans `[environment]` or `labels` values, so a `{{...}}` span there passes through literally and silently — the unknown-span validation never sees it either (`cistella:issues/3`, which took the QA seat down: literal `{{container-home}}` in `PATH`, `AGENTMUX_CONFIGURATION_DIRECTORY`, and `TMUX`). A template syntax that fails open in some fields is worse than no templates: authors reasonably expect uniform behavior. Fix now, while the only template-bearing profile in the fleet is the seat profile (already reverted to literals).

## What Changes

- One uniform rule: every free-text container-facing profile value expands — mount triples both sides (already), command argv (already), `env` values (new), and `labels` table values (new).
- Any `{{...}}` span appearing anywhere else in profile text (notably label keys, where expansion would destabilize matching) is a typed error, never a silent literal. `container_home` keeps its existing pre-canonicalization rejection.
- Substitution stays inside the existing pipeline order (parse → normalize-home → expand → validate): env/labels values are substituted before their existing value validations run, so post-substitution values keep every current guarantee; substituted text is never rescanned.
- Regression coverage for the exact failure: template-bearing env/labels profiles resolve fully, unknown and unterminated spans in env/labels are typed errors, and a live conduct asserts harness-observed env values plus label round-trip.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mounts`: the profile-schema requirement gains template expansion on `env` values and `labels` values, and fail-closed span handling on all remaining profile text. (The requirement lives under the `mounts` spec from the profile-resolution/template lineage.)

## Impact

- `src/profile.rs` (`template_values` + substitution targets, ~20 lines); unit matrix plus one live conduct asserting harness-observed env.
- No transport, runtime, registry, or CLI surface changes. Seat profile can re-adopt `{{container-home}}` env values after this lands.
- Process: verification probes must print every field touched, not just mounts (lesson recorded in `cistella:issues/3`).
