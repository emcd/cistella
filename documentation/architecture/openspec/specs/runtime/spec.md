# runtime Specification

## Purpose
TBD - created by archiving change add-cistella-driver. Update Purpose after archive.
## Requirements
### Requirement: Minted session lifecycle with labels and shared teardown
The driver SHALL manage one disposable container per session with labels `cistella.id`, `cistella.directory` (canonical host path), `cistella.profile`, `cistella.profile-digest` (sha256 of profile TOML), `cistella.identity`, `cistella.command` (argv array serialized as JSON array string `["opencode","--model","x"]`, lossless), `cistella.image` (resolved digest) plus generic `--label k=v` and optional profile `labels` table (both refuse keys beginning with `cistella.` and Quadlet-invalid `=`/`\n`/`\0`, only driver emits `cistella.*` last) and SHALL provide `conduct`/`survey`/`inspect`/`terminate`/`gc` verbs (no alias, `gc` abbreviation, `clear` rejected). `conduct --profile <name|path> [--directory <path>] [--identity <id>] [--label k=v]... [-- <command...>]` SHALL mint a time-sortable id (lowercase Crockford base32 ms+40 random bits, `[a-z0-9]` fixed length, >=40 bits entropy, `cistella-<id>.container`), generate a Quadlet `.container` unit (`Image` from profile resolved digest, `Label=cistella.*`, `Tmpfs=<canonical container_home>`, `SuccessExitStatus=143`) and own the harness lifetime (flock `$XDG_RUNTIME_DIR/cistella/lock` with fallback `/tmp/cistella.lock` if `XDG_RUNTIME_DIR` absent, held before any unit-file or scratch creation through `create unit -> start` and by `terminate`/`gc` around scan-and-teardown, create unit -> start -> `podman exec -i -t` harness argv after `--` or profile `command` array on pane PTY -> wait -> shared `teardown(id)` -> exit with harness status or `128+signal` after `SIGHUP`/`SIGTERM`, trapping `SIGHUP`/`SIGTERM`). `conduct` SHALL never pull images implicitly; a tag that does not resolve locally is a typed refusal naming the image ("pull it, or run check"). `survey` SHALL join registry `~/.config/containers/systemd/cistella-*.container` + `systemctl show ActiveState` + `podman ps` labels, supporting zero or many selectors as filters (`--directory` canonicalized, `--label k=v` ANDed, `cistella.` refused); `enter`/`inspect`/`terminate` SHALL each take exactly one selector (positional `<id>` unique prefix, or `--directory`, or `--label`, exclusive; zero/multiple matches are typed refusals listing candidates). `teardown(id)` SHALL stop the unit (fail closed unless `LoadState=not-found`) -> wait `ActiveState` inactive/failed -> remove `.container` (session id from `Label=` before removal) -> `daemon-reload` -> `reset-failed` -> remove scratch; `terminate` (any-state) and `gc` both call it. GC SHALL classify every candidate (unit files plus `podman ps` `cistella.id` labels, sorted) before mutating anything: absence is `podman container exists` exit 1, any other inspect/exists failure is a typed error reaping nothing. Unit values SHALL survive systemd specifiers and quoting: `Label=`/`Environment=` values are quoted with `%` doubled (`%h` arrives literal), while `Image=`/`Volume=`/`Tmpfs=` stay raw with `%` doubled.

#### Scenario: Label and GC
- **WHEN** a Quadlet unit `cistella-<id>.container` is generated and started and the session crashes (or external `systemctl stop`)
- **THEN** `systemctl --user show` shows inactive/failed, unit file remains, and `cistella gc` reaps the orphaned unit/file and scratch and `journalctl --user -u <service>` shows post-mortem

#### Scenario: GC isolation (never reap unrelated or active)
- **WHEN** an unrelated container without `cistella.*` labels, an active `cistella` session container (running with `cistella.*` labels), and an orphaned `cistella` unit exist
- **THEN** `cistella gc` reaps only the orphaned `cistella` unit whose `ActiveState` is inactive/failed/not-found and leaves the unrelated and active `running` containers untouched; on any `podman inspect` error `gc` SHALL fail closed

#### Scenario: gc during conduct creation reaps nothing
- **WHEN** `conduct` holds `flock` from before any creation through start and `gc` runs concurrently
- **THEN** `gc` reaps nothing

#### Scenario: conduct failure after install leaves no residue
- **WHEN** `conduct` fails after install (image missing, start fails)
- **THEN** `teardown` runs and no unit file or scratch remains, typed error propagates

#### Scenario: terminate while conduct attached converges
- **WHEN** `conduct` has an attached `enter` session and `terminate <id>` is run from another pane
- **THEN** `conduct`'s exec returns, `conduct`'s teardown finds unit gone/inactive and exits cleanly, neither errors

#### Scenario: Specifier-safe labels round-trip
- **WHEN** `conduct` runs with argv and directory containing `%h`, spaces, quotes, and `=`
- **THEN** the podman-side `cistella.command`/`cistella.directory` labels equal the unit-file values parsed as JSON/labels, byte-for-byte

#### Scenario: Inspect failure fails closed
- **WHEN** `podman inspect` fails for a candidate whose container still exists
- **THEN** `gc` returns a typed error and reaps nothing

### Requirement: Quadlet supervision
The driver SHALL use Quadlet systemd user units for long-lived supervision, not `podman generate systemd`. Units are `cistella-<id>.container` (no harness in name), `UserNS=keep-id`, `Tmpfs=<canonical container_home>`, `SuccessExitStatus=143`.

#### Scenario: Quadlet present
- **WHEN** `~/.config/containers/systemd/cistella-*.container` exists
- **THEN** `systemctl --user daemon-reload` creates the service

### Requirement: Full egress, no network isolation (credential-absence push)
The driver SHALL grant full network egress in V1 (model APIs require it) and SHALL document that no network isolation exists; push enforcement SHALL be credential-absence, never network policy.

#### Scenario: Egress documented
- **WHEN** `podman exec` `curl https://api.openai.com` is run
- **THEN** it succeeds, and `cistella` docs state `Network: full egress; push: credential-absence`

### Requirement: Rootless mapping suitable for keep-id
The driver SHALL require cgroup v2, the invoking user's `subuid`/`subgid` range (e.g., `100000:65536`), and `--userns=keep-id` so bind-mounted worktrees retain host ownership.

#### Scenario: keep-id ownership
- **WHEN** a worktree is bind-mounted with `--userns=keep-id`
- **THEN** `podman exec` `id -u` equals the invoking user's host UID and files created in the worktree are owned by that UID

### Requirement: AppArmor, not SELinux :z
The driver SHALL not use `:z`/`:Z` SELinux relabel flags on Ubuntu AppArmor hosts.

#### Scenario: Mount without SELinux flag
- **WHEN** a bind mount is created on Ubuntu
- **THEN** no `:z` is appended and `podman run` succeeds without `permission denied`

