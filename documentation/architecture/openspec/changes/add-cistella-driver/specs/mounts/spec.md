## ADDED Requirements

### Requirement: Declarative profile file with credential-surface slot
The driver SHALL use a declarative profile file (TOML, e.g., `coders.toml` profile) with required fields: `harness`, `mounts` (allowlist triples), `env`, and `credential-surface` slot (even if PoC hardcodes its value per `home:coordination/6`).

#### Scenario: Profile validation
- **WHEN** a profile with `harness = "opencode"` and `mounts = [...]` and `credential-surface = "none"` is loaded
- **THEN** validation succeeds; missing `credential-surface` fails

### Requirement: Allowlist-only mount triples with env exports and explicit HOME
The driver SHALL accept only an allowlist of explicit `(host-source, container-target, mode RO/RW)` triples per profile and SHALL export matching env vars (`HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, etc.) inside the container. `HOME` SHALL be set explicitly by the driver, consistent with the profile's container-targets (e.g., `/home/cistella`), and is part of the closed env list — not ambient from the host.

#### Scenario: Opencode profile (canonical)
- **WHEN** profile selects `~/.config/opencode`, `~/.local/share/opencode`, `~/.local/state/opencode`, worktree, per-session scratch (`/tmp/cistella-<session>`), and the seat's configured notebook repositories RW with config RO
- **THEN** only those paths are mounted, each as its triple, and env vars (`HOME`, `XDG_*`) point at the container targets

#### Scenario: HOME explicitly set
- **WHEN** `podman run --rm --userns=keep-id` is run without host `HOME` forwarded
- **THEN** `echo $HOME` inside is `/home/cistella` (or the profile's `container-home`) and `opencode --version` does not attempt `mkdir '/.local'`

#### Scenario: No parent mount
- **WHEN** a profile does not list a parent directory
- **THEN** no sibling paths are visible inside the container (no masking needed)

### Requirement: Per-harness shared state RW
The driver SHALL mount only the selected harness's shared state roots RW, preserving collaboration/resume semantics.

#### Scenario: Shared SQLite
- **WHEN** OpenCode `~/.local/share/opencode` is mounted RW
- **THEN** two sessions using OpenCode see the same history and `opencode --resume` works

### Requirement: Notebook mounts
The driver SHALL mount the seat's configured notebook repositories RW when needed by container-local `nb` MCP and notebook configuration RO.

#### Scenario: Notebook RW
- **WHEN** container runs `nb` MCP with the seat's `notebooks` list
- **THEN** each configured repository is RW and `~/.config/nb` is RO

### Requirement: Mount validation (canonicalize, reject unsafe/colliding)
The driver SHALL canonicalize both sides of every mount triple before validation (longest existing prefix, per `dispositor` `assert_disjoint_roots` precedent) and SHALL reject any profile where a container-target is at or above sensitive roots (`/`, `/etc`, etc.) or where two container-targets overlap (one is an ancestor of the other).

#### Scenario: Canonicalize and reject overlap
- **WHEN** a profile contains `host-source` `/tmp/link` (symlink to `/home/me/src`) and `container-target` `/work` + `container-target` `/work/src` overlap
- **THEN** validation fails with `overlapping mounts` before any container is created
