# mounts Specification

## Purpose
Mounts: synced from change tmpdir-simplification. Update Purpose after archive.
## MODIFIED Requirements
### Requirement: Allowlist-only mount triples with env exports and explicit HOME

The driver SHALL accept only an allowlist of explicit `(host-source, container-target, mode RO/RW)` triples from the profile plus zero or more `--mount <host>:<target>:<mode>` CLI triples (conduct only), and SHALL export matching env vars (`HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, etc.) inside the container. `HOME` SHALL be set explicitly by the driver from `container-home` (canonical), not ambient. The `directory` worktree mount is `host_directory:container-target:rw` where the container target comes from `--session-directory <host>[:<container>]` (hidden alias `--cwd`; default `/work` when the container side is omitted). CLI triples undergo the same validation as profile triples; a CLI triple with the exact canonical target of a profile triple overrides it, while exact-target CLI/profile collisions are typed errors; strict ancestor/descendant nesting stacks (deepest mount wins, parent-first order). No per-session scratch triple exists (framework knows nothing about `/tmp` — temp goes to the image rootfs or a tmpdir slice).

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

### Requirement: Nested-under-RO availability preflight

Overlap validation admits descendant triples over read-only ancestors (deepest mount wins); the OCI runtime honors them only when the full destination chain including the final mountpoint pre-exists in the ancestor's host source (preexists rule: the child bind resolves through the mounted parent against the host tree, so a missing link fails `mkdirat` inside the RO mount). After mount merge and canonicalization, before unit creation, `conduct` SHALL translate each descendant-under-RO container suffix onto its nearest already-ordered RO ancestor's canonical host source and require every chain component including the final mountpoint to exist as a directory; a missing link or wrong type is a typed error naming the RO ancestor and the missing translated host path, leaving no residue. Runc remains authoritative for host-tree races with existing fail-closed teardown covering them.

#### Scenario: Preexisting nested chain succeeds
- **WHEN** a session nests a target beneath an RO ancestor whose host source already contains the full destination chain including the final mountpoint
- **THEN** conduct proceeds, the child is writable, the parent stays RO

#### Scenario: Missing intermediate refuses pre-mutation
- **WHEN** any chain link including the final mountpoint is absent (or not a directory) in the ancestor's host source
- **THEN** conduct fails with a typed error naming the RO ancestor and the missing host path, before unit file creation (assert absent)

#### Scenario: Symlinked sources resolve before translation
- **WHEN** an ancestor host source traverses a symlink
- **THEN** the canonicalized source supplies the namespace and the check passes or fails on the resolved tree
