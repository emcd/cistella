## MODIFIED Requirements

### Requirement: Allowlist-only mount triples with env exports and explicit HOME

The driver SHALL accept only an allowlist of explicit `(host-source, container-target, mode RO/RW)` triples from the profile plus zero or more `--mount <host>:<target>:<mode>` CLI triples (conduct only), and SHALL export matching env vars (`HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, etc.) inside the container. `HOME` SHALL be set explicitly by the driver from `container-home` (canonical), not ambient. The profile schema additionally accepts optional `home-source-class` (`container` | `host`, absent meaning container-private) plus `home-host-source` path required exactly when class is `host`; validation constructs one authoritative home descriptor (canonical target + class + backing) bound into the compile snapshot fingerprint and consumed by the supervisor FULL derivation, the RO-retention sets, and the mount renderer, and a rendering from any other snapshot refuses at application. The `directory` worktree mount is `host_directory:container-target:rw` where the container target comes from `--session-directory <host>[:<container>]` (hidden alias `--cwd`; default `/work` when the container side is omitted). CLI triples undergo the same validation as profile triples; a CLI triple with the exact canonical target of a profile triple overrides it, while exact-target CLI/profile collisions are typed errors; strict ancestor/descendant nesting stacks (deepest mount wins, parent-first order). A container-private home target receives FULL Landlock rights folded into the FULL-route accounting; a host-class home is declared but never constructed in this release (explicit unsupported-construction refusal before any mutation).

#### Scenario: Writable-home profile validates

- **WHEN** a profile sets `home-source-class = "container"`
- **THEN** validation passes and the home target enters FULL-grant accounting

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
- **WHEN** profile selects `~/.config/opencode`, `~/.local/share/opencode`, `~/.local/state/opencode`, directory at `/work`, and the seat's configured notebook repositories RW with config RO
- **THEN** only those paths are mounted, each as its triple, and env vars (`HOME`, `XDG_*`) point at the container targets

#### Scenario: HOME explicitly set
- **WHEN** `conduct` starts a session with `container-home = "/home/cistella"` and without host `HOME` forwarded
- **THEN** `echo $HOME` inside is `/home/cistella` as set by the driver from `container-home` and `opencode --version` does not attempt `mkdir '/.local'`

#### Scenario: No parent mount
- **WHEN** a profile does not list a parent directory
- **THEN** no sibling paths are visible inside the container (no masking needed)

#### Scenario: Home source class declares provenance
- **WHEN** a profile sets `home-source-class = "container"` with no host source, or omits the field on the current topology
- **THEN** conduct constructs the private tmpfs home and the home target enters FULL-grant accounting via the bound descriptor; a supplied host source with absent/`container` class, a `host` class without path, or a `host` class with path each fail with their typed refusal before any mutation
