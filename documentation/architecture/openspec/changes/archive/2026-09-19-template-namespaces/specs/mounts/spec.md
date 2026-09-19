# mounts Specification Delta

## MODIFIED Requirements

### Requirement: Declarative profile file with image, command, directory and identity

The driver SHALL resolve a profile reference as follows: a reference containing `/` or ending in `.toml` is a filesystem path; otherwise it is a name resolved in order — configuration directory (`--configuration-directory <dir>` flag, then `$CISTELLA_CONFIGURATION_DIRECTORY`, each naming `<dir>/profiles/<name>.toml` as a closed tier: only the first supplied directory is consulted (the flag shadows the env var), a missing name is a typed error with no fallthrough, and supplied tiers are never seeded) — then `${XDG_CONFIG_HOME}/cistella/profiles/<name>.toml` (XDG honored, default `~/.config`) — then baked-in examples (`data/profiles/*.toml` compiled with `include_str!`, version-locked with the binary). The cwd `data/profiles/` lookup and the `CARGO_MANIFEST_DIR` fallback SHALL NOT exist, and no development-directory detection of any kind SHALL exist. `data/profiles/*.toml` are examples, not per-system profiles. When a named lookup reaches the default tier, the driver SHALL ensure the XDG profiles dir exists and seed any baked example missing there (never overwriting user files); explicit paths and supplied configuration-directory tiers never trigger seeding. Seeding happens on the `conduct` path, the sole profile consumer. The profile schema is unchanged: required `image` (tag or digest, resolved digest recorded as `cistella.image`) and `mounts` (allowlist triples with `host-source`, `container-target`, `mode` keys, supporting `{{core:container-home}}`, `{{core:host-home}}`, and `{{core:project-name}}` template expansions on both sides); optional `container-home` (default `/home/cistella`, early qualified expansion — allowlisted `environment:`, supplied `supplement:` — resolved before canonicalization as specified below while bare, unknown-context, and `core:` spans are refused before canonicalization, then canonicalized exactly as literals today with non-absolute paths, control characters, and sensitive roots `/`, `/etc` etc rejected), `environment` (keys `[A-Z_][A-Z0-9_]*`, values `=`/`\n` rejected, values supporting `{{core:container-home}}`, `{{core:host-home}}`, and `{{core:project-name}}` template expansions), `credential-surface` slot, `command` (TOML array argv, never shell string, serialized as JSON array string for `cistella.command`), and optional `labels` table (keys/values refuse `cistella.` prefix and Quadlet-invalid `=`/`\n`/`\0`, values supporting `{{core:container-home}}`, `{{core:host-home}}`, and `{{core:project-name}}` template expansions, `{{...}}` spans in keys typed errors, only driver emits `cistella.*` last); generic `--label k=v` CLI and profile `labels` share one validation/precedence rule (refuse `cistella.`, only driver emits, CLI wins). `HOME` SHALL be derived from `container-home` (canonical), not freely overridden via `[environment]`. `directory` (`--session-directory` with hidden `--cwd` alias, optional defaults to canonical `cwd`, pair form `<host>[:<container>]`) replaces `worktree`. Conduct accepts `--project-name <name>` (default: basename of the canonical session directory); unknown contexts, unknown names, or unterminated spans in any profile field are typed errors; bare `{{name}}` spans without a context are typed errors directing to qualified spellings and substituted values are never rescanned. Conduct canonicalizes the session directory before resolution and passes the validated project name in; context-free resolution without a project name resolves template-free profiles literally and fails template-bearing profiles with a typed error (never deriving from cwd). The registry name recorded in `cistella.profile` is the reference for names and the file stem for paths (paths never enter labels).
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
