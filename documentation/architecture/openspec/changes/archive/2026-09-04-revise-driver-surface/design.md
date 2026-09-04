## Context

`add-cistella-driver` (`debf148` + `8b9c583`) ships a working driver with 20/20 `nextest` and live `run → cistella exec → stop → gc` verification on `spark-f980` (Podman 4.9.3 rootless, cgroup v2, `~/.config/containers/systemd/`). Operator and Advisor reviews converged: the surface still mimics Podman (`run` prints "started", caller chains `run && exec`), mixes nouns/verbs (`doctor`/`logs`/`status` vs `run`/`stop`), requires caller-supplied `--session-id`/`--worktree`/`--seat`/`--harness` that leaks Agentmux bundle-scoped names and harness knowledge, and does not own the harness lifetime. The next step is a breaking surface revision pre-1.0 with no aliases.

## Goals / Non-Goals

**Goals:**
- All-Latinate verb slate (`conduct`/`enter`/`survey`/`inspect`/`terminate`/`gc` with `check` abbreviation/foreign exemption) naming the driver's job, not Podman's; `gc` kept, no alias, `clear`/`clean` rejected.
- `conduct` mints the session id (lowercase `[a-z0-9]` fixed length, time-sortable, >=40 bits entropy) and owns the harness lifetime (create unit → exec harness on pane PTY → wait → shared teardown, `flock` on `$XDG_RUNTIME_DIR/cistella/lock` held from install through start and by `terminate`/`gc` around scan-and-teardown).
- Cistella knows only profiles (image tag/digest, `container_home`, mounts, credential_surface, env, optional `command` array, optional `labels`); harness is argv after `--` chosen by caller (Agentmux).
- Directory/identity renames (`--directory` optional defaults to canonical `cwd`, `--identity` label not credential selector) and generic `--label` passthrough for Agentmux correlation, selector-based addressing with unique prefix and canonical directory.

**Non-Goals:**
- Kitty graphics via tmux, multi-runtime Docker/VM, harness registry beyond profile name, network proxy — deferred to `cistella:todos/*`.
- Backwards compatibility — hard break, no deprecated aliases (alpha).

## Decisions

- **Verbs: all-Latinate.** `conduct` (`conducere` "lead through to end") owns whole lifetime vs `run` (names nothing) or `commence` (names start only); `enter` (companion shell), `survey` (registry join, `list` is Germanic `lista` via French, `clear` Latin `clarus` but `gc` is abbreviation and precise `reclaim unreachable residue`, so `gc` kept with no alias per M1), `inspect` (labels + journald, `podman logs` fallback is typed error if journald unavailable), `terminate` (any-state external teardown), `gc` (reaps orphaned unit files + scratch, not containers, Quadlet `--rm` makes `podman ps` invisible). `check` (foreign) and `gc` (abbreviation) allowed via exemption per Operator; `--cwd` alias for `--directory` dropped per S1 (`--directory` optional defaults to `cwd`, avoids misleading `cwd` alias). Alternative all-Germanic (`run`/`list`/`go-in`/`clean`) rejected for colliding `list` and vague `clean` vs precise `gc`.

- **Session identity: mint, not hash.** `hash(worktree+profile)` is a config digest; two concurrent invocations on same worktree/profile would collide and Quadlet `--replace` would silently kill the first. Session is an invocation, ephemeral by construction (tmpfs `HOME` `src/runtime.rs:66`), so no stable id needed. `conduct` mints lowercase Crockford base32 `ms + 40 random bits` fixed length, `[a-z0-9]` time-sortable, >=40 bits entropy (`cistella-<id>.container`), `Label=cistella.id`, `cistella.directory` (canonical host path), `cistella.profile`, `cistella.profile-digest` (sha256 of profile TOML), `cistella.identity`, `cistella.command` (argv array serialized as JSON array string `["opencode","--model","x"]`, lossless, not shell-escaped), `cistella.image` (resolved digest, I6), plus generic `--label k=v` passthrough and optional profile `labels` table where both refuse `cistella.` prefix (reserved, only driver emits) and Quadlet-invalid (`=`/`\n`/`\0`) and no-newline guard (M2). Addressing via `<id>` unique prefix or selectors `--directory`/`--label` (grammar: `enter`/`inspect`/`terminate` take exactly one of `<id>` or `--directory` or `--label k=v`, exclusive, `--directory` canonicalized, `--label` split on first `=`, I2/I4; `survey` takes same flags as filters permitting zero or many); ambiguous prefix/selector is typed refusal listing candidates (I2). Creation-window race (M3) between `conduct` install and `gc` scan is closed by `flock` on `$XDG_RUNTIME_DIR/cistella/lock` (fallback `/tmp` if `XDG_RUNTIME_DIR` absent, per scratch) **acquired before any unit-file or scratch creation** and held through `create unit -> start` and by `terminate`/`gc` around scan-and-teardown; scenario "gc during conduct creation reaps nothing" tested at each creation phase. `cistella.command` round-trip (`argv -> label JSON -> argv` equals input, including spaces/quotes/`=`) is tested.

- **Harness knowledge removed.** Profile `harness` field and `cistella.harness` label removed; unit drops harness (`cistella-<id>.container`), image lives in profile (`image = "localhost/cistella-...@sha256:..."`), CLI `--image` only override. Driver never interprets profile name; `data/profiles/<name>.toml` shipped and resolved by name/path, retiring synthetic TOML-by-interpolation `src/main.rs:137` and its injection guard. Alternative keep `harness` as metadata rejected as leakage.

- **Directory/identity renames.** `--worktree` (`worktree` is Git-worktree-specific, now clones-of-clones) -> `--directory` (Latin, optional defaults to canonicalized `cwd`, no `--cwd` alias per S1), `--seat` -> `--identity` (Latin, label in V1, not credential selector per S2; credential surface remains profile-driven `src/identity.rs:48`, Phase 2 binds identity to credential). Generic `--label` passthrough lets Agentmux stamp `agentmux.session`/`agentmux.bundle` without Cistella learning Agentmux vocabulary (`agentmux:todos/runtime/26` now `{{session_id}}` only after `directory` default).

- **Conduct owns teardown, shared teardown.** `terminate`/`gc` both call `teardown(id)` = `systemctl --user stop` (fail closed unless `LoadState=not-found` via `show`) → wait `ActiveState` inactive/failed (poll 20×100ms) → remove `.container` (session id resolved via `Label=` before removal, fixes `98d01d1` orphan scratch) → `daemon-reload` → `reset-failed` → remove `XDG_RUNTIME_DIR/cistella/<id>` (preferred over `/tmp`, per-user tmpfs, same lifetime as user manager; fallback `/tmp/cistella-<id>`). `conduct` traps `SIGHUP`/`SIGTERM` and on failure after install runs `teardown` then propagates typed error (S3), exit code is harness status or `128+signal` after `SIGHUP`/`SIGTERM` (S5), `terminate` from another pane while `conduct` attached converges without error (S4). Alternative keep separate `stop`/`gc` paths rejected as "two paths for one mutation" (`home:procedures/design/1` I3).

## Risks / Trade-offs

- **`conduct` blocking the pane** → Mitigation: trap `SIGHUP`/`SIGTERM`, wait on harness `podman exec` PID, `0x03` in-band via raw tty (spike-verified).
- **Minted id not human-memorable** → Mitigation: `survey` orders naturally (time-sortable) and selectors (`--directory .`) avoid typing id; `cistella enter --directory .` for companion pane.
- **No hash stability for "re-attach"** → Mitigation: intentional — sessions are not re-attachable by config; use `survey` selectors to find active session for same directory.

## Migration Plan

- Breaking CLI: `coders.toml` changes `cistella run --session-id X --harness Y --worktree Z` → `cistella conduct --profile <name> --directory Z --identity I --label agentmux.session={{session_id}} -- <harness-cmd>`; Agentmux interpolation added. No data migration; existing units are ephemeral. Rollback: `systemctl --user stop` + `rm` + `daemon-reload`.

## Open Questions

- `conduct` vs `host` naming if `conduct` reads oddly in `coders.toml`.
- Whether `--allow-shared-worktree` guard for second session on same directory is needed in V1.
