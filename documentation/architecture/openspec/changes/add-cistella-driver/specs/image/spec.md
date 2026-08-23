## ADDED Requirements

### Requirement: Debian base with baked terminfo
The driver image SHALL be `debian:bookworm-slim` with `ncurses-bin` (provides `tic`, `infocmp`, `tput`) and `tic -x` baked entries for `xterm-ghostty` (sourced versioned from Ghostty releases — vendored `terminfo/xterm-ghostty` file with release version recorded, or `infocmp -x` from a pinned Ghostty version) and `tmux-256color` (plus `xterm-kitty` as needed) into `/usr/share/terminfo`.

#### Scenario: Ghostty terminfo present
- **WHEN** `podman run` the driver image with `TERM=xterm-ghostty` and without `TERMINFO` env or host bind-mount
- **THEN** `infocmp xterm-ghostty` succeeds inside and `tput colors` is `256`

#### Scenario: Host bind-mount not required
- **WHEN** host is Ghostty `TERMINFO=/usr/local/share/terminfo` with `g/ghostty` + `x/xterm-ghostty`
- **THEN** container `infocmp` succeeds without `-v /usr/local/share/terminfo:/usr/local/share/terminfo:ro`

### Requirement: Version pinning and host prerequisites
The driver SHALL pin `debian:bookworm-slim` and SHALL depend on host prerequisites `podman`, `uidmap`, `slirp4netns`, `fuse-overlayfs`, cgroup v2, and the invoking user's `subuid`/`subgid` range (e.g., `100000:65536`) in `/etc/subuid`/`/etc/subgid`.

#### Scenario: Host preflight
- **WHEN** `scripts/validate-rootless-podman` (pc-setup) is run
- **THEN** it verifies command presence, cgroup v2, `subuid`/`subgid` entries for the invoking user, `podman info` `rootless:true` `cgroupVersion:v2` `storageDriver:overlay` `networkBackend:netavark`, `podman unshare`, and `~/.config/containers/systemd/` exists (implemented locally in pc-setup, not yet pushed; contract is SHALL)
