# runtime Specification Delta

## MODIFIED Requirements

### Requirement: Minted session lifecycle with labels and shared teardown


The driver SHALL manage one disposable container per session with labels `cistella.id`, `cistella.directory` (canonical host path), `cistella.profile`, `cistella.profile-digest` (sha256 of profile TOML), `cistella.identity`, `cistella.command` (argv array serialized as JSON array string `["opencode","--model","x"]`, lossless), `cistella.image` (resolved digest) plus generic `--label k=v` and optional profile `labels` table (both refuse keys beginning with `cistella.` and Quadlet-invalid `=`/`\n`/`\0`, only driver emits `cistella.*` last) and SHALL provide `conduct`/`survey`/`inspect`/`terminate`/`gc` verbs (no alias, `gc` abbreviation, `clear` rejected). `conduct --profile <name|path> [--directory <path>] [--identity <id>] [--label k=v]... [-- <command...>]` SHALL mint a time-sortable id (lowercase Crockford base32 ms+40 random bits, `[a-z0-9]` fixed length, >=40 bits entropy, `cistella-<id>.container`), generate a Quadlet `.container` unit (`Image` from profile resolved digest, `Label=cistella.*`, `Tmpfs=<canonical container-home>`, `RunInit=true`, `SuccessExitStatus=143`) and own the harness lifetime (flock `$XDG_RUNTIME_DIR/cistella/lock` with fallback `/tmp/cistella.lock` if `XDG_RUNTIME_DIR` absent, held before any unit-file or scratch creation through `create unit -> start` and by `terminate`/`gc` around scan-and-teardown, nested-RO preflight (where applicable) -> create unit -> start -> shared mountpoint preparation (lexical ancestor candidates from user mounts, de-duplicated, then runtime containment proof per candidate against the resolved emitted bind set — fully resolved path must not lie beneath any bind-mount target, tmpfs-home subtrees excepted as container-ephemeral; `mkdir -p` plus conditional seat-uid chown of proved candidates only, mount roots and `/` excluded, internal scratch/credential mounts seeding no candidates, fail closed via teardown) -> `podman exec -i -t` harness argv after `--` or profile `command` array on pane PTY -> wait -> shared `teardown(id)` -> exit with harness status or `128+signal` after `SIGHUP`/`SIGTERM`, trapping `SIGHUP`/`SIGTERM`). `conduct` SHALL never pull images implicitly; a tag that does not resolve locally is a typed refusal naming the image ("pull it, or run check"). `survey` SHALL join registry `~/.config/containers/systemd/cistella-*.container` + `systemctl show ActiveState` + `podman ps` labels, supporting zero or many selectors as filters (`--directory` canonicalized, `--label k=v` ANDed, `cistella.` refused); `enter`/`inspect`/`terminate` SHALL each take exactly one selector (positional `<id>` unique prefix, or `--directory`, or `--label`, exclusive; zero/multiple matches are typed refusals listing candidates). `teardown(id)` SHALL stop the unit promptly via init-forwarded SIGTERM (no `StopTimeout` exhaustion, no SIGKILL fallback in the clean path) (fail closed unless `LoadState=not-found`) -> wait `ActiveState` inactive/failed -> remove `.container` (session id from `Label=` before removal) -> `daemon-reload` -> `reset-failed` -> remove scratch; `terminate` (any-state) and `gc` both call it. GC SHALL classify every candidate (unit files plus `podman ps` `cistella.id` labels, sorted) before mutating anything: absence is `podman container exists` exit 1, any other inspect/exists failure is a typed error reaping nothing. Unit values SHALL survive systemd specifiers and quoting: `Label=`/`Environment=` values are quoted with `%` doubled (`%h` arrives literal), while `Image=`/`Volume=`/`Tmpfs=` stay raw with `%` doubled.
#### Scenario: Sibling creation under auto-created parents
- **WHEN** a session mounts a deep target whose parents do not exist and the harness creates a sibling directory under those parents
- **THEN** creation succeeds (parents prepared seat-owned); mount roots, `/`, and RO subtree contents are unaltered. Normative: RO applies to the mounted subtree, which is never chowned; only the non-mounted ancestor chain gains traversal/creation rights as traversal infrastructure.

#### Scenario: Preparation failure fails closed
- **WHEN** mountpoint preparation fails after start
- **THEN** `conduct` tears down the session and reports a typed error; no half-prepared session persists

#### Scenario: Symlinked ancestor into a mount is refused
- **WHEN** a lexical candidate resolves (symlink components followed, bind targets resolved alongside) to a path beneath a bind-mount target
- **THEN** preparation fails closed with a typed error before any mkdir/chown; no host-backed mount is mutated

#### Scenario: Internal mounts seed no candidates
- **WHEN** a session carries scratch and credential-surface volumes alongside user mounts
- **THEN** their ancestors (`/tmp`, `/run`) are never chowned; only user-mount chains are prepared

#### Scenario: Descendant paths beneath bind mounts are excluded
- **WHEN** a session carries an unrelated read-only bind mount alongside a tmpfs-nested deep target
- **THEN** every path beneath the RO mount is excluded from preparation even though none equals the mountpoint (RO content byte-untouched, mount root never chowned); tmpfs-home subtrees are the explicit exception (container-ephemeral) and remain eligible. (A nested-under-RO *target* cannot start — the OCI runtime cannot mkdir inside the RO parent — so that topology is covered by unit exclusion tests, and parse-time rejection of it is deliberately deferred: the session-directory triple routinely nests under RO profile triples with host-preexisting intermediates, which a parser cannot distinguish from the broken case.)
#### Scenario: Preflight precedes mutation
- **WHEN** a nested-under-RO session is conducted
- **THEN** the availability preflight runs before unit-file and scratch creation in the normative sequence
