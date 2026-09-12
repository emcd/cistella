# mounts Specification

## Purpose
TBD - created by archiving change add-cistella-driver. Update Purpose after archive.
## Requirements
### Requirement: Declarative profile file with image, command, directory and identity
The driver SHALL resolve a profile reference as follows: a reference containing `/` or ending in `.toml` is a filesystem path; otherwise it is a name resolved in order — configuration directory (`--configuration-directory <dir>` flag, then `$CISTELLA_CONFIGURATION_DIRECTORY`, each naming `<dir>/profiles/<name>.toml` as a closed tier: only the first supplied directory is consulted (the flag shadows the env var), a missing name is a typed error with no fallthrough, and supplied tiers are never seeded) — then `${XDG_CONFIG_HOME}/cistella/profiles/<name>.toml` (XDG honored, default `~/.config`) — then baked-in examples (`data/profiles/*.toml` compiled with `include_str!`, version-locked with the binary). The cwd `data/profiles/` lookup and the `CARGO_MANIFEST_DIR` fallback SHALL NOT exist, and no development-directory detection of any kind SHALL exist. `data/profiles/*.toml` are examples, not per-system profiles. When a named lookup reaches the default tier, the driver SHALL ensure the XDG profiles dir exists and seed any baked example missing there (never overwriting user files); explicit paths and supplied configuration-directory tiers never trigger seeding. Seeding happens on the `conduct` path, the sole profile consumer. The profile schema is unchanged: required `image` (tag or digest, resolved digest recorded as `cistella.image`) and `mounts` (allowlist triples); optional `container_home` (default `/home/cistella`, canonicalized, sensitive roots `/`, `/etc` etc rejected, `..` traversal rejected), `env` (keys `[A-Z_][A-Z0-9_]*`, values `=`/`\n` rejected), `credential_surface` slot, `command` (TOML array argv, never shell string, serialized as JSON array string for `cistella.command`), and optional `labels` table (keys/values refuse `cistella.` prefix and Quadlet-invalid `=`/`\n`/`\0`, only driver emits `cistella.*` last); generic `--label k=v` CLI and profile `labels` share one validation/precedence rule (refuse `cistella.`, only driver emits, CLI wins). `HOME` SHALL be derived from `container_home` (canonical), not freely overridden via `env`. `directory` (`--session-directory` with hidden `--cwd` alias, optional defaults to canonical `cwd`, pair form `<host>[:<container>]`) replaces `worktree`. The registry name recorded in `cistella.profile` is the reference for names and the file stem for paths (paths never enter labels).

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

### Requirement: Allowlist-only mount triples with env exports and explicit HOME
The driver SHALL accept only an allowlist of explicit `(host-source, container-target, mode RO/RW)` triples from the profile plus zero or more `--mount <host>:<target>:<mode>` CLI triples (conduct only), and SHALL export matching env vars (`HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, etc.) inside the container. `HOME` SHALL be set explicitly by the driver from `container_home` (canonical), not ambient. The `directory` worktree mount is `host_directory:container_target:rw` where the container target comes from `--session-directory <host>[:<container>]` (hidden alias `--cwd`; default `/work` when the container side is omitted). CLI triples undergo the same validation as profile triples; a CLI triple with the exact canonical target of a profile triple overrides it, while ancestor/descendant CLI/profile overlap outside the RO-ancestor rule is a typed error.

#### Scenario: Directory pair form mounts worktree at host path
- **WHEN** `conduct --session-directory /home/me/src/cistella:/home/me/src/cistella --profile <path>` runs
- **THEN** the worktree mounts RW at its canonical host path inside, `cistella.directory` still records the canonical host dir, and omitting `:<container>` mounts at `/work` as before

#### Scenario: Session directory rename with cwd alias
- **WHEN** `conduct --cwd /home/me/src/cistella` runs (alias of `--session-directory`)
- **THEN** the session conducts identically to the spelled-out flag; `--help` lists only `--session-directory`

#### Scenario: CLI mount triple unions with profile mounts
- **WHEN** `conduct --mount /home/me/Dropbox/Notes/cistella:/home/me/Dropbox/Notes/cistella:rw` runs with a profile that has no overlapping triple
- **THEN** the session starts with both the profile triples and the CLI triple mounted

#### Scenario: CLI mount with exact profile target overrides it
- **WHEN** a `--mount` triple has the same canonicalized container target as a profile triple
- **THEN** the CLI triple (source and mode) wins for the session, mirroring the CLI-wins label rule

#### Scenario: CLI mount partially overlapping profile mount fails closed
- **WHEN** a `--mount` triple is the ancestor or descendant (outside the RO-ancestor rule) of a profile triple's canonicalized container target
- **THEN** validation fails before any container is created

#### Scenario: Duplicate CLI mount triples fail closed
- **WHEN** two `--mount` triples share the same canonicalized container target
- **THEN** validation fails before any container is created (no last-wins)

#### Scenario: CLI mount on the worktree target fails closed
- **WHEN** a `--mount` triple's canonicalized container target equals the session-directory worktree target
- **THEN** validation fails before any container is created; the `--session-directory` pair form stays the only worktree-target knob

#### Scenario: Opencode profile (canonical)
- **WHEN** profile selects `~/.config/opencode`, `~/.local/share/opencode`, `~/.local/state/opencode`, directory at `/work`, per-session scratch (`XDG_RUNTIME_DIR/cistella/<id>` with fallback `/tmp/cistella-<id>`, `Label=cistella.id`), and the seat's configured notebook repositories RW with config RO
- **THEN** only those paths are mounted, each as its triple, and env vars (`HOME`, `XDG_*`) point at the container targets

#### Scenario: HOME explicitly set
- **WHEN** `conduct` starts a session with `container_home = "/home/cistella"` and without host `HOME` forwarded
- **THEN** `echo $HOME` inside is `/home/cistella` as set by the driver from `container_home` and `opencode --version` does not attempt `mkdir '/.local'`

#### Scenario: No parent mount
- **WHEN** a profile does not list a parent directory
- **THEN** no sibling paths are visible inside the container (no masking needed)

### Requirement: Mount validation (canonicalize, reject unsafe/colliding, two-tier topology)
The driver SHALL canonicalize both sides of every mount triple and `container_home` before validation (longest existing prefix, `..` cleaning, `dispositor` precedent) and SHALL reject any profile where `container_home` or a container-target is at or above sensitive roots (`/`, `/etc`, etc.), where two triples overlap (one is ancestor of the other) except a descendant triple over a read-only ancestor (deepest mount wins, mounted in depth order), or where a triple shadows session-home (`container_target` ancestor-or-equal of `container_home`). Triples MAY nest under the single distinguished writable session-home root at the explicit `HOME` (e.g., `/home/cistella` as `Tmpfs`, declared via `container_home`), which is a driver primitive, not a triple and not subject to peer-disjointness. Mount ordering: session-home first, then triples by path depth. `host_source`/`container_target` with `=`/`\n`/`\0` SHALL be rejected (Quadlet injection, M4).

#### Scenario: Read-only parent with writable child stacks
- **WHEN** a profile (plus CLI triples) mounts `~/Dropbox/Notes` RO and `~/Dropbox/Notes/cistella` RW
- **THEN** validation succeeds, the parent mounts first and the child over it, and the project repo is writable while siblings stay read-only

#### Scenario: Writable overlapping mounts still rejected
- **WHEN** two triples overlap and the ancestor is RW, or either triple shadows session-home
- **THEN** validation fails with `overlapping mounts` or the session-home error before any container is created

#### Scenario: Canonicalize and reject overlap
- **WHEN** a profile contains `host-source` `/tmp/link` (symlink to `/home/me/src`) and `container-target` `/work` + `container-target` `/work/src` overlap, or `container_home` `/home/cistella/../../etc`
- **THEN** validation fails with `overlapping mounts` or `sensitive root` before any container is created
