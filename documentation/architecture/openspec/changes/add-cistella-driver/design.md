## Context

Cistella is a new Rust CLI (`cistella v0.1.0`, edition 2024) that will own one OCI container per agent session. No driver exists; `openspec/specs/` is empty, `src/main.rs` is a scaffold, and the side spike `src/bin/terminal-spike.rs:1` (`ffeb89c`, `cistella:artifacts/pty/1`) is the only evidence. Host is Ubuntu 24.04 with Ghostty `xterm-ghostty` `TERMINFO=/usr/local/share/terminfo` and GNOME Terminal `xterm-256color`; tmux `tmux-256color` is the multiplexer. Podman 4.9.3 rootless is now available via `pc-setup` (`--userns=keep-id`, cgroup v2, `~/.config/containers/systemd/`). Stakeholders: Agentmux (coder profile consumer), `pc-setup` (host prerequisites, image layers), Advisor (design altitude), Reviewer General.

## Goals / Non-Goals

**Goals:**
- Reproducible devcontainer driver with declarative profiles, allowlist mounts, and `podman exec -i -t` transport that survives `tmux`/Pty.
- Binary pass/fail transport criteria from `home:coordination/6` observed via spike.
- Credential-absence push enforcement and per-seat signing.

**Non-Goals:**
- Kitty graphics fidelity via tmux (blocked by `allow-passthrough`; requires raw PTY/Ghostty — deferred).
- Multi-runtime (`--userns` is Podman-specific; Docker/VM abstraction is interface-only for V1).
- Agent identity registry beyond hardcoded test seat key + `allowed_signers` (this change uses hardcoded key; registry projection is a later change).
- Config library `home:ideas/projects/4` (defer until 3 consumers).

## Decisions

- **Runtime: rootless Podman only for V1, driver abstracts.** For spike: `podman run -d --userns=keep-id` with `--label cistella-*`; for V1 driver: `run` generates a Quadlet `.container` unit and `systemctl --user start` it (systemd owns the container, not `podman run`). No `--rm` — removal is `gc`'s job (`--rm` would auto-remove the exited container, destroying the `gc`/`logs` post-mortem). Quadlet user units, not `podman generate systemd`. AppArmor, not SELinux `:z`. Alternative Docker rejected until concrete need; VM isolation deferred. *Rationale:* spike is green on Podman; abstraction keeps `coders.toml` stable.
- **Transport: host owns PTY, `podman exec -i -t` with stdio on the session PTY slave.** Never `podman attach` to PID 1 (kills on stream death). Host `TIOCSWINSZ` → `SIGWINCH` → runtime resize API. Spike proved `isatty`/`winsize` `0 0` piped is negative control; `50 191 → 30 100` in throwaway tmux window with `-i -t` is positive, and `-t` alone is deaf for inbound (`DA1`/`0x03` need `-i`). Alternative `podman attach` rejected for durability.
- **Env forwarding (closed list):** `TERM` + `COLORTERM` (+ `TERM_PROGRAM` as closed) only; never `TERMINFO` with baked images (overrides baked `xterm-ghostty`). Podman does not forward implicitly — spike saw `TERM=tmux-256color` missing without `-e`. Alternative broad `XDG` mount rejected.
- **Terminfo: bake, not mount.** `tic -x` `xterm-ghostty` + `tmux-256color` (+ `xterm-kitty`/`wezterm`/`alacritty` as needed) into `/usr/share/terminfo` in `debian:bookworm-slim`. Spike: host `TERMINFO=/usr/local/share/terminfo` with `g/ghostty` + `x/xterm-ghostty` required `podman run -v /usr/local/share/terminfo:/usr/local/share/terminfo:ro -e TERMINFO=...` to make `xterm-ghostty` `present`; without mount stock Debian is `absent`. Paths vary (`/usr/share/terminfo` vs `/lib/terminfo`), so mounting is brittle. Alternative host bind-mount kept as `--mount-terminfo` experiment in spike only.
- **Mounts: allowlist-only triples `(host-source, container-target, mode)` with env exports.** No `sibling masking` (deny-list) — under allowlist there is no parent to mask. Spike: `~/.config/opencode`, `~/.local/share/opencode`, worktree, `nb` RW, config RO. Alternative broad `XDG` RO parent rejected.
- **Identity: per-seat SSH agent `AF_UNIX` mount + `allowed_signers` verification in PoC.** Socket is a normal agent; `sign-only` is a property of GitHub key registration (type `Signing` refuses auth) plus absence of any auth-registered credential in the container. `kill -INT` sanity and PTY `0x03` harness prove trap path; `forkpty` harness that plays terminal (`\033[c` → `\033[?6c`) proves reply-path. No Git/GitHub auth in containers.
- **Images: seat-agnostic, no per-user derived images.** All per-seat/per-user variation enters at container-create time, declared in the profile. Never build per-user derived images. Run-time tailoring that replaces build-time tailoring: (1) uid identity via `--userns=keep-id` plus Podman's synthesized `passwd`/`group` entry (templatable via `--passwd-entry`), (2) `HOME` set explicitly by the driver (closed env contract, consistent with profile container-targets), (3) writable `HOME` mount (tmpfs or session scratch) at `$HOME` with profile triples layered inside. The harness gets a sane, user-owned, ephemeral home whose only durable contents are the allowlist mounts. *Alternatives considered:* a finishing pass that builds a user-tailored derived layer adding `/etc/passwd`/`/etc/group` entries and user-owned paths (as used in a pre-`userns` devcontainers system). Rejected for V1: it creates an `N-seats × M-harnesses` build matrix, goes stale on every base update times every seat, and destroys single-answer image provenance (the digest pinning just established). The keep-id defect's root cause was user state written at build time; per-user derived images double down on that class. Escape hatch (not V1): if a harness proves impossible to install user-agnostically (hard-coded `$HOME`-relative paths), a cached derived layer keyed on `(base digest, uid)` MAY be added — deterministic and cheap — but do not implement until such a harness exists.

## Risks / Trade-offs

- **`-t` without `-i` half-working deaf session** → Mitigation: driver invariant is unconditional `-i -t` with stdio on PTY slave; spike is negative control.
- **Terminfo staleness if host adds new TERM** → Mitigation: bake common set; document driver image update when host TERM changes.
- **Large-payload flow control via stacked PTYs** → Mitigation: 1 MB `sha256` byte-integrity proven; Kitty graphics still needs raw PTY validation (deferred).
- **Resize `0 0` when piped** → Mitigation: harness enforces `tmux`/`forkpty` with `isatty:true` and `stty size` non-zero.
- **No network isolation in V1 (full egress)** → Mitigation: document explicitly — V1 grants full egress (model APIs require it); push enforcement is credential-absence, never network policy.
- **V1 accepted-risk blast radius: model-provider credentials + shared harness state RW** → Mitigation: explicit threat model; per-harness shared state RW preserves resume but means one compromised session can corrupt shared state for that harness; version bumps require drain-and-migrate (stop matching harness sessions, backup `~/.local/share/<harness>`, migrate with one session, validate, resume).

## Migration Plan

- Ship spike as `cargo run --bin terminal-spike` regression fixture (and `cistella:artifacts/pty/1`). Driver `0.1.0` → `0.2.0` adds `run`/`gc`; no data migration. Rollback: `podman rm -f` + `gc`.

## Open Questions

- `TERM_PROGRAM` closed-list content (forward as-is vs strip?).
- Whether `wezterm`/`alacritty` need baking for V1 or on demand.
