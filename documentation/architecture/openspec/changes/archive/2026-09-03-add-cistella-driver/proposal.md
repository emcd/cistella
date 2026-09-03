## Why

Agent harnesses (OpenCode, etc.) need reproducible, auditable, safe containerization with broad in-container permissions. No driver exists that implements the devcontainers model for agent harnesses: declarative per-session profiles, allowlist-only mounts, runtime-agnostic integration, and credential-absence push enforcement. The terminal transport spike (`src/bin/terminal-spike.rs:1`, `cistella:artifacts/pty/1`, commit `ffeb89c`) proved the control-plane risks are solvable with explicit invariants (`podman exec -i -t` on PTY slave, closed env list, baked terminfo, `--userns=keep-id`, SIGWINCH via resize API, byte-integrity).

## What Changes

- Introduce `cistella` CLI driver: one OCI container per agent session, invoked as session command via `coders.toml` profile (e.g., Agentmux coder profile). Lifecycle verbs: `run`/`stop`/`status`/`logs`/`gc` with runtime labels (`session-id`, `seat`, `harness`, `profile`).
- Rootless Podman for V1; driver abstracts runtime for Docker/VM later. `--userns=keep-id`, Quadlet systemd user units, AppArmor.
- Transport: host owns terminal multiplexer PTY/tmux; container detached (`podman run -d`); harness via `podman exec -i -t` with stdio on the session PTY slave (never `podman attach` to PID 1). Correct `TERM`/`COLORTERM` forwarding, baked `xterm-ghostty` + `tmux-256color` terminfo, `TIOCGWINSZ`/`SIGWINCH` propagation, `isatty`, exit-status, signal (`0x03` → `SIGINT`), and large-payload integrity.
- Mount model: allowlist-only explicit triples `(host-source, container-target, mode RO/RW)` with env exports (`XDG_*` etc.); no masking; per-harness shared state RW preserved; notebook repos RW for container-local `nb` MCP, config RO.
- Identity: per-seat SSH signing keys + `allowed_signers` for signed review commits (hardcoded test seat key for this change; registry projection is a later change); no Git/GitHub write auth in containers; host-side review/merge/push.
- Image: `debian:bookworm-slim` base with `tic -x` baked terminfo, `ncurses-base`, and host prerequisites via `pc-setup` (Podman, `uidmap`, `slirp4netns`, `fuse-overlayfs`, cgroup v2).

## Capabilities

### New Capabilities
- `transport`: PTY allocation, `isatty`/`winsize`, `TERM`/`COLORTERM` forwarding, `infocmp`/`tput`, CSI/OSC/Kitty escape pass-through, DA1 reply-path, SIGINT via `0x03`, exit-status, resize, large-payload integrity.
- `runtime`: rootless container lifecycle (`podman run -d --userns=keep-id` for spike; Quadlet unit generation + start for V1 driver, without `--rm` — removal is `gc`), labels, `gc` reaping, `podman exec -i -t` wiring, cgroup/storage/network validation, full egress documented (no network isolation; push is credential-absence, not network policy).
- `mounts`: allowlist schema, triple validation, env export, per-harness shared-state handling, notebook RW/RO.
- `identity`: per-seat signing (`sign-only` SSH agent `AF_UNIX` bind-mount), `allowed_signers` verification, credential-absence enforcement.
- `image`: OCI base layer, terminfo baking (`xterm-ghostty`, `tmux-256color` via `tic -x`), `ncurses` deps, version pinning.

### Modified Capabilities
- None — no existing specs; `openspec/specs/` is empty.

## Impact

- New Rust crate `cistella` with `src/main.rs` CLI and `src/bin/terminal-spike.rs` retained as regression fixture (and `cistella:artifacts/pty/1` as evidence).
- Depends on `pc-setup` host prerequisites (Podman rootless, `~/.config/containers/systemd/`, `subuid`/`subgid`).
- Affects `Agentmux` coder profiles (session command), `pc-setup` bootstrap, and future driver consumers. No breaking API yet (new project, `0.1.0`).
