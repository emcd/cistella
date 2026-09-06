## MODIFIED Requirements

### Requirement: Declarative profile file with image, command, directory and identity
The driver SHALL resolve a profile reference as follows: a reference containing `/` or ending in `.toml` is a filesystem path; otherwise it is a name resolved in order — configuration directory (`--configuration-directory <dir>` flag, then `$CISTELLA_CONFIGURATION_DIRECTORY`, each naming `<dir>/profiles/<name>.toml` as a closed tier: a missing name is a typed error with no fallthrough and supplied tiers are never seeded) — then `${XDG_CONFIG_HOME}/cistella/profiles/<name>.toml` (XDG honored, default `~/.config`) — then baked-in examples (`data/profiles/*.toml` compiled with `include_str!`, version-locked with the binary). The cwd `data/profiles/` lookup and the `CARGO_MANIFEST_DIR` fallback SHALL NOT exist, and no development-directory detection of any kind SHALL exist. `data/profiles/*.toml` are examples, not per-system profiles. When a named lookup reaches the default tier, the driver SHALL ensure the XDG profiles dir exists and seed any baked example missing there (never overwriting user files); explicit paths and supplied configuration-directory tiers never trigger seeding. Seeding happens on the `conduct` path, the sole profile consumer. The profile schema is unchanged: required `image` (tag or digest, resolved digest recorded as `cistella.image`) and `mounts` (allowlist triples); optional `container_home` (default `/home/cistella`, canonicalized, sensitive roots `/`, `/etc` etc rejected, `..` traversal rejected), `env` (keys `[A-Z_][A-Z0-9_]*`, values `=`/`\n` rejected), `credential_surface` slot, `command` (TOML array argv, never shell string, serialized as JSON array string for `cistella.command`), and optional `labels` table (keys/values refuse `cistella.` prefix and Quadlet-invalid `=`/`\n`/`\0`, only driver emits `cistella.*` last); generic `--label k=v` CLI and profile `labels` share one validation/precedence rule (refuse `cistella.`, only driver emits, CLI wins). `HOME` SHALL be derived from `container_home` (canonical), not freely overridden via `env`. `directory` (`--directory` Latinate, optional defaults to canonical `cwd`, no `--cwd` alias per S1) replaces `worktree`. The registry name recorded in `cistella.profile` is the reference for names and the file stem for paths (paths never enter labels).

#### Scenario: Profile validation
- **WHEN** a profile with `mounts = [...]` and `image = "localhost/cistella-opencode@sha256:..."` and `container_home = "/home/cistella"` is loaded
- **THEN** validation succeeds; missing `image` fails; `container_home` traversal `/home/cistella/../../etc` fails

#### Scenario: Profile labels reserve cistella prefix
- **WHEN** a profile `labels` table contains `cistella.id = "spoof"` or CLI `--label cistella.id=spoof`
- **THEN** validation fails with `reserved prefix` before any container is created

#### Scenario: Name resolves without a source checkout
- **WHEN** `conduct --profile default` runs from a directory containing no `data/profiles` (e.g. `$HOME`)
- **THEN** the baked `default` example resolves (seeding `~/.config/cistella/profiles/default.toml` when absent) and the session starts

#### Scenario: Supplied configuration directory wins over XDG
- **WHEN** `--configuration-directory /tmp/mine` holds `opencode.toml` with a custom image, `$CISTELLA_CONFIGURATION_DIRECTORY` names another dir, and an XDG copy exists
- **THEN** `conduct --profile opencode` uses the flag dir's image; with only the env dir set it uses the env dir's image; neither falls through and neither is seeded

#### Scenario: User copy wins over baked
- **WHEN** `~/.config/cistella/profiles/opencode.toml` exists with a custom image alongside the baked example
- **THEN** `conduct --profile opencode` uses the XDG copy's image

#### Scenario: Explicit path bypasses search
- **WHEN** `conduct --profile /tmp/custom.toml` runs
- **THEN** that file loads directly and `cistella.profile` records `custom` (file stem)

#### Scenario: Supplied configuration directory is closed
- **WHEN** `conduct --configuration-directory /tmp/empty-profiles --profile default` runs and that dir holds no `default.toml`
- **THEN** the driver refuses with a typed error naming the profile; it does not fall through to XDG or baked examples, writes nothing to the XDG profiles dir, and seeds nothing there
