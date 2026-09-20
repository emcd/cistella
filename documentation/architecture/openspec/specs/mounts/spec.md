# mounts Specification

## Purpose
TBD - created by archiving change add-cistella-driver. Update Purpose after archive.
## Requirements
### Requirement: Declarative profile file with image, command, directory and identity

The driver SHALL resolve a profile reference as follows: a reference containing `/` or ending in `.toml` is a filesystem path; otherwise it is a name resolved in order — configuration directory (`--configuration-directory <dir>` flag, then `$CISTELLA_CONFIGURATION_DIRECTORY`, each naming `<dir>/profiles/<name>.toml` as a closed tier: only the first supplied directory is consulted (the flag shadows the env var), a missing name is a typed error with no fallthrough, and supplied tiers are never seeded) — then `${XDG_CONFIG_HOME}/cistella/profiles/<name>.toml` (XDG honored, default `~/.config`) — then baked-in examples (`data/profiles/*.toml` compiled with `include_str!`, version-locked with the binary). The cwd `data/profiles/` lookup and the `CARGO_MANIFEST_DIR` fallback SHALL NOT exist, and no development-directory detection of any kind SHALL exist. `data/profiles/*.toml` are examples, not per-system profiles. When a named lookup reaches the default tier, the driver SHALL ensure the XDG profiles dir exists and seed any baked example missing there (never overwriting user files); explicit paths and supplied configuration-directory tiers never trigger seeding. Seeding happens on the `conduct` path, the sole profile consumer. The profile schema is unchanged: required `image` (tag or digest, resolved digest recorded as `cistella.image`) and `mounts` (allowlist triples with `host-source`, `container-target`, `mode` keys, supporting `{{core:container-home}}`, `{{core:host-home}}`, and `{{core:project-name}}` template expansions on both sides); optional `container-home` (default `/home/cistella`, early qualified expansion — allowlisted `environment:`, supplied `supplement:` — resolved before canonicalization as specified below while bare, unknown-context, and `core:` spans are refused before canonicalization, then canonicalized exactly as literals today with non-absolute paths, control characters, and sensitive roots `/`, `/etc` etc rejected), `environment` (keys `[A-Z_][A-Z0-9_]*`, values `=`/`\n` rejected, values supporting `{{core:container-home}}`, `{{core:host-home}}`, and `{{core:project-name}}` template expansions), `credential-surface` slot, `command` (TOML array argv, never shell string, serialized as JSON array string for `cistella.command`), and optional `labels` table (keys/values refuse `cistella.` prefix and Quadlet-invalid `=`/`\n`/`\0`, values supporting `{{core:container-home}}`, `{{core:host-home}}`, and `{{core:project-name}}` template expansions, `{{...}}` spans in keys typed errors, only driver emits `cistella.*` last); generic `--label k=v` CLI and profile `labels` share one validation/precedence rule (refuse `cistella.`, only driver emits, CLI wins). `HOME` SHALL be derived from `container-home` (canonical), not freely overridden via `[environment]`. `directory` (`--session-directory` with hidden `--cwd` alias, optional defaults to canonical `cwd`, pair form `<host>[:<container>]`) replaces `worktree`. Conduct accepts `--project-name <name>` (default: basename of the canonical session directory); unknown contexts, unknown names, or unterminated spans in any profile field are typed errors; bare `{{name}}` spans without a context are typed errors directing to qualified spellings and substituted values are never rescanned. Conduct canonicalizes the session directory before resolution and passes the validated project name in; context-free resolution without a project name resolves template-free profiles literally and fails template-bearing profiles with a typed error (never deriving from cwd). The registry name recorded in `cistella.profile` is the reference for names and the file stem for paths (paths never enter labels).

#### Scenario: Renamed keys parse, old keys fail
- **WHEN** a profile uses `[environment]`, `credential-surface`, `container-home`, `host-source`, and `container-target`
- **THEN** resolution succeeds; a profile using the pre-rename spellings fails closed at parse time with no silent compat shim

#### Scenario: Migration note
- **WHEN** an operator upgrades with pre-rename seeded XDG copies
- **THEN** the migration note maps every old key to its new spelling and gives the re-seed path (delete the seeded file; the next conduct re-seeds the new example)

#### Scenario: Qualified namespaces expand
- **WHEN** a profile uses `{{core:*}}`, `{{supplement:*}}` (supplied via `--supplement`), and `{{environment:*}}` (allowlisted and present in host env) spans
- **THEN** all three resolve under the existing ordering, precedence, laziness, and no-rescan rules

#### Scenario: Bare, unknown, and unpermitted spans fail closed
- **WHEN** a profile uses a bare `{{name}}` span, an unknown context, an unsupplied supplement name, or an environment name absent from the allowlist
- **THEN** resolution fails with a typed error naming the offense (allowlist-absent names name the opt-in knob); no literal span text reaches the container

#### Scenario: Credential-shaped environment names are hard-refused
- **WHEN** a profile references `{{environment:<credential-shaped>}}` (e.g. `SSH_AUTH_SOCK`, a `*_TOKEN` name) even if allowlisted
- **THEN** resolution fails with a typed hard-refusal error distinct from the allowlist-absent diagnostic; no lookup occurs and no literal span text reaches the container


### Requirement: Early container-home expansion
`container-home` SHALL expand allowlisted `{{environment:*}}` and supplied `{{supplement:*}}` spans in a pre-resolution phase before canonicalization and sensitive-root validation; `{{core:*}}` spans inside `container-home` are typed cycle errors (self-reference). No-rescan applies to the substituted value. Template-free literals pass through this phase unchanged.

#### Scenario: Portable container-home from host HOME
- **WHEN** a profile sets `container-home = '{{environment:HOME}}'` with `HOME` allowlisted and present
- **THEN** `container-home` resolves to the host `HOME` value before canonicalization, and `{{core:container-home}}` expands against the validated result

#### Scenario: Substituted traversal canonicalizes like literals
- **WHEN** early expansion of `container-home` yields safe `.`/`..` segments (e.g. `/home/me/../me`)
- **THEN** the value canonicalizes identically to the same literal input, and conduct proceeds

#### Scenario: Dangerous substituted values rejected
- **WHEN** early expansion of `container-home` yields a non-absolute path, control characters, or a value canonicalizing to a sensitive root (`/`, `/etc`)
- **THEN** conduct fails with a typed error before any mutation

#### Scenario: Self-referential container-home rejected
- **WHEN** `container-home` contains a `{{core:*}}` span
- **THEN** resolution fails with a typed cycle error before any lookup
### Requirement: Allowlist-only mount triples with env exports and explicit HOME
The driver SHALL accept only an allowlist of explicit `(host-source, container-target, mode RO/RW)` triples from the profile plus zero or more `--mount <host>:<target>:<mode>` CLI triples (conduct only), and SHALL export matching env vars (`HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, etc.) inside the container. `HOME` SHALL be set explicitly by the driver from `container-home` (canonical), not ambient. The `directory` worktree mount is `host_directory:container-target:rw` where the container target comes from `--session-directory <host>[:<container>]` (hidden alias `--cwd`; default `/work` when the container side is omitted). CLI triples undergo the same validation as profile triples; a CLI triple with the exact canonical target of a profile triple overrides it, while exact-target CLI/profile collisions are typed errors; strict ancestor/descendant nesting stacks (deepest mount wins, parent-first order).

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
- **WHEN** `conduct` starts a session with `container-home = "/home/cistella"` and without host `HOME` forwarded
- **THEN** `echo $HOME` inside is `/home/cistella` as set by the driver from `container-home` and `opencode --version` does not attempt `mkdir '/.local'`

#### Scenario: No parent mount
- **WHEN** a profile does not list a parent directory
- **THEN** no sibling paths are visible inside the container (no masking needed)

### Requirement: Mount validation (canonicalize, reject unsafe/colliding, two-tier topology)
The driver SHALL canonicalize both sides of every mount triple and `container-home` before validation (longest existing prefix, `..` cleaning, `dispositor` precedent) and SHALL reject any profile where `container-home` or a container-target is at or above sensitive roots (`/`, `/etc`, etc.), where two triples share an exact canonical target, or where a triple shadows session-home (`container-target` ancestor-or-equal of `container-home`). Strict ancestor/descendant triples always stack (deepest mount wins, mounted in depth order: parents before children). Nesting under read-only ancestors additionally requires the nested-ro preflight (preexisting host chain, typed refusal otherwise); nesting under read-write ancestors proceeds by `mkdir` through the parent. Triples MAY nest under the single distinguished writable session-home root at the explicit `HOME` (e.g., `/home/cistella` as `Tmpfs`, declared via `container-home`), which is a driver primitive, not a triple and not subject to peer-disjointness. Mount ordering: session-home first, then triples by path depth. `host-source`/`container-target` with `=`/`\n`/`\0` SHALL be rejected (Quadlet injection, M4).

#### Scenario: Read-only parent with writable child stacks
- **WHEN** a profile (plus CLI triples) mounts `~/Dropbox/Notes` RO and `~/Dropbox/Notes/cistella` RW
- **THEN** validation succeeds, the parent mounts first and the child over it, and the project repo is writable while siblings stay read-only

#### Scenario: Read-write nesting stacks in any mode combination
- **WHEN** triples nest in RW-under-RW, RO-under-RW, or RO-under-RO combinations (e.g. a wholesale `~/src` RW ancestor with a pair-form worktree beneath it)
- **THEN** validation succeeds and podman mounts parents before children; only exact duplicate targets refuse

#### Scenario: Exact duplicate targets still rejected
- **WHEN** two triples share a canonical target, or either triple shadows session-home
- **THEN** validation fails with `duplicate mount target` or the session-home error before any container is created

#### Scenario: Canonicalize and reject unsafe targets
- **WHEN** a profile contains `host-source` `/tmp/link` (symlink to `/home/me/src`) with an identical canonical source elsewhere, or `container-home` `/home/cistella/../../etc`
- **THEN** validation fails with `overlapping host_sources` (identical canonical sources stay refused) or `sensitive root` before any container is created

### Requirement: Nested-under-RO availability preflight
Overlap validation admits descendant triples over read-only ancestors (deepest mount wins); the OCI runtime honors them only when the full destination chain including the final mountpoint pre-exists in the ancestor's host source (preexists rule: the child bind resolves through the mounted parent against the host tree, so a missing link fails `mkdirat` inside the RO mount). After mount merge and canonicalization, before unit or scratch creation, `conduct` SHALL translate each descendant-under-RO container suffix onto its nearest already-ordered RO ancestor's canonical host source and require every chain component including the final mountpoint to exist as a directory; a missing link or wrong type is a typed error naming the RO ancestor and the missing translated host path, leaving no residue. Runc remains authoritative for host-tree races with existing fail-closed teardown covering them.

#### Scenario: Preexisting nested chain succeeds
- **WHEN** a session nests a target beneath an RO ancestor whose host source already contains the full destination chain including the final mountpoint
- **THEN** conduct proceeds, the child is writable, the parent stays RO

#### Scenario: Missing intermediate refuses pre-mutation
- **WHEN** any chain link including the final mountpoint is absent (or not a directory) in the ancestor's host source
- **THEN** conduct fails with a typed error naming the RO ancestor and the missing host path, before unit file or scratch creation (assert both absent)

#### Scenario: Symlinked sources resolve before translation
- **WHEN** an ancestor host source traverses a symlink
- **THEN** the canonicalized source supplies the namespace and the check passes or fails on the resolved tree
