## 1. Runtime and image

- [x] 1.1 Add `debian:bookworm-slim` Dockerfile with `ncurses-bin` (provides `tic`/`infocmp`/`tput`) and `tic -x` baked `xterm-ghostty` (versioned from Ghostty releases, pinned) + `tmux-256color` to `/usr/share/terminfo`
- [x] 1.2 Wire `pc-setup` host preflight (`scripts/validate-rootless-podman` SHALL check command presence, cgroup v2, invoking-user `subuid`/`subgid`, `podman info` `rootless:true`/`cgroupVersion:v2`/`overlay`/`netavark`, `podman unshare`, and `~/.config/containers/systemd/` — implemented locally in pc-setup per `a6f83a4f`, not yet pushed)
- [x] 1.3 Implement `podman run -d --userns=keep-id --label cistella.*` for spike and Quadlet `.container` unit generation + `systemctl --user start` for V1 (no `--rm`; removal is `gc`)

## 2. Transport

- [x] 2.1 Implement `podman exec -i -t` with stdio on session PTY slave (tmux pane / `forkpty` master), never `podman attach`
- [x] 2.2 Forward closed env list `TERM`/`COLORTERM`/`TERM_PROGRAM` via `-e`; ensure `TERMINFO` is not forwarded with baked images
- [x] 2.3 Wire `TIOCSWINSZ`/`SIGWINCH` via runtime resize API and verify `stty size` before/after `tmux resize-window`
- [x] 2.4 Verify `isatty`, `infocmp`/`tput colors 256`, CSI `1b 5b 33 34 6d`, `DA1 \033[c` → `\033[?6c` reply-path, and `0x03` → `SIGINT` via `forkpty` harness
- [x] 2.5 Verify `exit 42` passthrough and 1 MB `sha256` large-payload integrity

## 3. Mounts

- [x] 3.1 Implement allowlist-only triple schema `(host-source, container-target, mode)` with declarative profile file (`harness`, `mounts`, `env`, `credential-surface` slot) and env exports
- [x] 3.2 Add Opencode profile mounts (`~/.config/opencode`, `~/.local/share/opencode`, `~/.local/state/opencode`, worktree, per-session scratch) and seat's configured `nb` repositories RW/RO (canonical list per mounts spec)

## 4. Identity and push

- [x] 4.1 Mount per-seat `AF_UNIX` `SSH_AUTH_SOCK` RO (normal agent; sign-only is GitHub key type `Signing` + no auth-registered credential) and provision `allowed_signers`
- [x] 4.2 Verify `git commit -S` host-side `ssh-keygen -Y verify` and enforce no `GITHUB_TOKEN`/`ssh -T` inside

## 5. Driver CLI and GC

- [x] 5.1 Implement `cistella run`/`stop`/`status`/`logs`/`gc` with `session-id`/`seat`/`harness`/`profile` labels and `cistella gc` reaping
- [x] 5.2 Add `tests/integration/transport.rs` that invokes the PTY harness (`forkpty` DA1/`0x03`, `isatty`/`stty size`, `exit 42`, 1 MB `sha256`) and keep `src/bin/terminal-spike.rs` as manual `cargo run --bin terminal-spike` regression command

## 6. Validation

- [x] 6.1 Run the built driver image with `TERM=xterm-ghostty` and without `--mount-terminfo` or `TERMINFO` (baked `xterm-ghostty` must be present) and verify `infocmp`/`tput colors 256` inside; keep `--mount-terminfo` only as optional diagnostic
- [x] 6.2 Run `cargo nextest` and `cargo clippy -- -D warnings` green on `debian:bookworm-slim` with `TERM=xterm-ghostty`
