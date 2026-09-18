## Why

The profile schema grew with truncated and snake_case TOML spellings (`[env]`, `credential_surface`, `container_home`, `host_source`, `container_target`) against the project's full-words, hyphenated-keys convention. Every profile in the fleet (baked examples, seeded XDG copies, dogfood profiles, test fixtures) carries the old spellings. Rename once, now, while the seat count is minimal — later only gets more expensive.

## What Changes

- **BREAKING** Table `[env]` → `[environment]` (Rust field renamed to match; all `profile.env` references updated).
- **BREAKING** Hyphenated TOML keys via serde `rename_all`: `credential-surface`, `container-home`, `host-source`, `container-target` (top-level and triple keys alike; Rust fields unchanged).
- Both renames land in one change with a single migration note, so profiles break exactly once: rename keys in place, no semantic change.
- Baked examples (`data/profiles/*.toml`), all test-fixture TOML, and the dogfood profiles (dev draft, XDG `opencode`) move to the new spellings in the same change.
- Seeded XDG copies are never overwritten by the driver: operators with pre-rename seeded files must re-seed (delete + conduct) or rename keys by hand. Old spellings fail closed at parse time (no silent compat shim — a shim would double the schema surface permanently).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mounts`: the profile-schema requirement's spellings change (`[environment]`, hyphenated keys); the migration note is part of the requirement text.
- `identity`: `credential_surface` spellings in prose/examples change to `credential-surface`.
- `runtime`: prose references to the profile field change to the `container-home` key spelling.

## Impact

- `src/profile.rs` serde attributes only (plus `MountTriple`); no behavior, validation, or expansion logic changes.
- Every inline-TOML test fixture rewritten; full suite must pass unchanged in behavior.
- Docs: subsystem README or profile examples mentioning old keys updated alongside.
