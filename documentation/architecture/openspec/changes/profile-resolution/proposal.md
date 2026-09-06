## Why

Profile lookup resolves bare names against `data/profiles/` relative to the caller's cwd, falling back to the compile-time `CARGO_MANIFEST_DIR`. Both break outside a source checkout: an installed binary has no source tree nearby, and the manifest path names the build machine. The driver needs a production-ready search path before any real adoption (including our own dogfood).

## What Changes

- **BREAKING** Name resolution order becomes: explicit filesystem path (reference containing `/` or ending `.toml`, unchanged) → configuration directory (`--configuration-directory <dir>` flag or `$CISTELLA_CONFIGURATION_DIRECTORY`, see below) → `${XDG_CONFIG_HOME}/cistella/profiles/<name>.toml` (XDG honored, default `~/.config`) → baked-in examples by name. The cwd `data/profiles/` lookup and the `CARGO_MANIFEST_DIR` fallback are removed, and there is no magic development-directory detection.
- A configuration directory names `<dir>/profiles/<name>.toml` and is a **closed** tier: a missing name there is a typed error with no fallthrough to XDG or baked (a typo'd custom name must never silently resolve to a baked example). Supplied tiers are never seeded. Development profiles live wherever the developer wants (operator convention: `.auxiliary/configuration/cistella/profiles/`, gitignored as per-host local config) and are passed explicitly.
- `data/profiles/*.toml` are declared examples/templates, baked into the executable with `include_str!` (Agentmux precedent) and version-locked with the binary.
- When a named lookup reaches the default tier, the driver ensures the XDG profiles dir exists and seeds any baked example missing there (never overwrites user files); an explicit path that does not exist is an error, supplied configuration-directory tiers never trigger seeding, and scaffolding never happens. Seeding happens on the `conduct` path, the sole profile consumer.
- Developer profiles (e.g. the dogfood `cistella-dev` profile) live under `.auxiliary/configuration/cistella/profiles/`, not `data/profiles/`.

## Capabilities

### New Capabilities

- (none — resolution behavior, not a new capability)

### Modified Capabilities

- `mounts`: the declarative-profile-file requirement gains the resolution order, the examples-baked-in rule, and the seed-if-absent bootstrap.

## Impact

- Affects `src/profile.rs` (`resolve`, new bootstrap + XDG helpers), `src/cli.rs` + `src/main.rs` (new `--configuration-directory` flag and `$CISTELLA_CONFIGURATION_DIRECTORY` tier, profile-name reporting already uses the file stem), `data/profiles/` (reworded as examples), `tests/unit/mount.rs` + `tests/integration/labels.rs` fixtures, and the mounts spec.
- No CLI surface change to `--profile <name|path>` itself; adds `--configuration-directory <dir>` (conduct only, the sole profile consumer) plus `$CISTELLA_CONFIGURATION_DIRECTORY`. Name shadowing changes (XDG now beats baked examples; supplied configuration directories are closed and shadow nothing implicitly) are documented in the requirement.
