## Why

The `add-cistella-driver` driver works but its surface mimics Podman (`run` prints "started", caller must chain `run && exec`, `--session-id`/`--harness`/`--worktree`/`--seat` leak bundle-scoped Agentmux names and harness knowledge into Cistella, verbs mix nouns and verbs, and `run` does not own the harness lifetime. The next step before Operator adoption is to make `conduct` own the session, mint unique ids, use all-Latinate verbs, and make Cistella know only profiles.

## What Changes

- **BREAKING** Rename verbs to all-Latinate with `gc`/`check` abbreviation/foreign exemptions: `run` -> `conduct`, `status` -> `survey`, `logs` -> `inspect`, `doctor` -> `check`, `stop` -> `terminate`, `exec` -> `enter`, `gc` kept with no alias; no aliases.
- **BREAKING** `conduct` mints the session id (lowercase Crockford base32 ms+40 random bits, fixed length, `[a-z0-9]` time-sortable, `cistella-<id>.container`, `Label=cistella.id` + `cistella.directory` canonical host path + `cistella.profile` + `cistella.profile-digest` + `cistella.identity` + `cistella.command` argv + `cistella.image` resolved digest, plus generic `--label k=v` passthrough with `cistella.` prefix refused) and owns the harness lifetime: create unit -> start -> `podman exec -i -t` harness argv after `--` (or profile `command`) on pane PTY -> wait -> shared `teardown(id)` -> exit with harness status (traps `SIGHUP`/`SIGTERM`, `0x03` in-band, advisory `flock` on `$XDG_RUNTIME_DIR/cistella/lock` held from install through start and by `terminate`/`gc` around scan-and-teardown to avoid creation-window race).
- **BREAKING** Directory/identity renames: `--worktree` -> `--directory` (optional, defaults to canonicalized `cwd`, no `--cwd` alias), `--seat` -> `--identity` (label, not credential selector; credential surface remains profile-driven), generic `--label k=v` passthrough for Agentmux correlation (`agentmux.session`, `agentmux.bundle`), selectors `enter`/`inspect`/`terminate` take `<id>` prefix or `--directory`/`--label` with directory canonicalization and unique-prefix/ambiguous typed refusal.
- **BREAKING** Remove `harness` field/flag and `cistella.harness` label; replace with `cistella.command` (argv) and `image` lives in profile (`image = "localhost/cistella-...@sha256:..."`), CLI `--image` only override. Unit name drops harness: `cistella-<id>.container`, `Tmpfs=<canonical container_home>` (not literal `/home/cistella`).
- **BREAKING** Profile schema: `image` (tag or digest, required) + `mounts` (required) + `container_home` (optional, default `/home/cistella`) + `env` (optional) + `credential_surface` + optional `command` (TOML array argv, never shell string) + optional `labels` table; shipped `data/profiles/<name>.toml` resolved by name/path, retiring synthetic TOML-by-interpolation in `src/main.rs:137`. `--label` is CLI flag, not profile field unless `labels` table is used.
- Harden `conduct` hash vs mint decision per Advisor `aaff94d4`: mint (invocation identity, not config digest) to avoid Quadlet `--replace` killing concurrent sessions on same worktree/profile.

## Capabilities

### New Capabilities
- (none — surface revision)

### Modified Capabilities
- `transport`: `conduct` and `enter` own `podman exec -i -t` on PTY slave, closed `TERM` forwarded at exec time; command after `--` is harness chosen by caller, fallback to profile `command` array; `conduct` exit code is harness status or 128+signal after `SIGHUP`/`SIGTERM` teardown.
- `runtime`: minted id (`[a-z0-9]` lowercase, fixed length, time-sortable, >=40 bits entropy), `cistella-<id>.container` naming, `Label=cistella.id/directory/profile/profile-digest/identity/command/image` plus generic `--label` ( `cistella.` prefix refused, values `=`/`\n` rejected), `survey`/`terminate`/`gc` via registry (`~/.config/containers/systemd/cistella-*.container` + `systemctl show` + `podman ps`), shared `teardown` (stop -> wait ActiveState -> remove file -> daemon-reload -> reset-failed -> remove scratch, `flock` on `$XDG_RUNTIME_DIR/cistella/lock` held from install through start and by `terminate`/`gc` around scan-and-teardown), `SuccessExitStatus=143`, `Tmpfs=<canonical container_home>`.
- `mounts`: profile schema with `image` (required), `command` (array), `directory` mount at `/work`, `container_home` canonicalization (`..` traversal, sensitive roots, `=`/`\n` injection), `directory`/`identity` renames, `HOME` derives from `container_home`.
- `identity`: `cistella.identity` label (renamed from `cistella.seat`, not credential selector), `cistella.command` label (renamed from `cistella.harness`), `credential_surface` still via profile.

## Impact

- Affects `src/cli.rs`, `src/main.rs`, `src/runtime.rs`, `src/transport.rs`, `src/profile.rs`, `src/mount.rs`, `src/identity.rs`, `src/preflight.rs`, `tests/integration/cli_lifecycle.rs`, `data/profiles/`, `coders.toml` integration (Agentmux `{{directory}}`/`{{session_id}}` interpolation via `agentmux:todos/runtime/26`).
- Breaking CLI surface pre-1.0, no deprecated aliases.
