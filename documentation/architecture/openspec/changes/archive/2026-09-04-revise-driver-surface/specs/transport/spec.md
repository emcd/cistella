## MODIFIED Requirements

### Requirement: Transport uses host PTY with podman exec -i -t on PTY slave via conduct/enter
The driver SHALL run `podman exec -i -t` with stdio on the session PTY slave (tmux pane or Agentmux Pty slave) and SHALL never use `podman attach` to PID 1. `conduct` SHALL own the harness lifetime: create unit -> start -> `podman exec -i -t` harness argv after `--` (or profile `command` array) on the pane PTY -> wait -> shared teardown, exiting with harness status or `128+signal` after `SIGHUP`/`SIGTERM`. `enter` SHALL provide companion shell entry using the same transport, with exactly one selector per invocation — positional `<id>` unique prefix, or `--directory <path>`, or one or more `--label k=v` (ANDed, `cistella.` refused, canonicalized), mixing forms is usage error and zero/multiple matches are typed refusals listing candidates.

#### Scenario: Piped exec is deaf
- **WHEN** `podman exec` is run with piped stdio
- **THEN** `isatty` is false and `stty size` is `0 0` and inbound `DA1`/`0x03` are deaf

#### Scenario: PTY slave exec is live
- **WHEN** `podman exec -i -t` is run with stdio on the pane PTY slave (via `conduct` harness or `enter`)
- **THEN** `isatty` is true, `stty size` reflects the host window, and `DA1`/`0x03` traverse

#### Scenario: Selector ambiguous is typed refusal
- **WHEN** `enter --directory .` matches two sessions on same directory
- **THEN** driver refuses with typed error listing candidates (I2)

### Requirement: Env forwarding is closed
The driver SHALL forward `TERM` and `COLORTERM` (and `TERM_PROGRAM` as closed) via `-e` at exec time for both `conduct` and `enter` and SHALL NOT forward `TERMINFO` with baked images. `conduct` SHALL NOT bake `TERM` into the unit; `enter` SHALL forward closed env at exec time.

#### Scenario: Ghostty outside
- **WHEN** host is `TERM=xterm-ghostty` `TERMINFO=/usr/local/share/terminfo`
- **THEN** container sees `TERM=xterm-ghostty` and `infocmp` succeeds from baked `/usr/share/terminfo/x/xterm-ghostty`, without `TERMINFO` env

#### Scenario: Podman does not forward implicitly
- **WHEN** driver runs `podman exec` without `-e TERM`
- **THEN** container `TERM` is unset and `infocmp` fails

### Requirement: Resize propagates via SIGWINCH
The driver SHALL propagate `TIOCSWINSZ` on the host master as `SIGWINCH` to the `podman exec -i -t` client and via the runtime resize API to the container PTY.

#### Scenario: Live resize
- **WHEN** tmux window is resized `80x24 → 100x30` while `podman exec -i -t` `stty size` is running on the pane slave via `conduct`/`enter`
- **THEN** container `stty size` changes `24 80 → 30 100` (spike `50 191 → 30 100`)

#### Scenario: Initial-size on re-exec
- **WHEN** a new `podman exec -i -t` is started after a resize
- **THEN** its initial `stty size` equals the current host window size

### Requirement: Exit-status passthrough
The driver SHALL propagate the harness exit status via `conduct` exit code (from `podman exec`).

#### Scenario: Exit 42
- **WHEN** `conduct` runs harness `sh -c 'exit 42'` (argv after `--`)
- **THEN** `conduct` exits `42`

#### Scenario: Pane closed
- **WHEN** `conduct` runs under a detached tmux and `kill-pane` sends `SIGHUP`
- **THEN** `conduct` tears down (remove unit, scratch) and exits `128+1` unless harness had already exited (then harness status)

### Requirement: Signal delivery via 0x03
The driver SHALL deliver `0x03` written to the host PTY master as `SIGINT` to the container foreground group via the slave line discipline (`isig`, `intr=^C`).

#### Scenario: 0x03 becomes SIGINT
- **WHEN** container runs `trap "touch /tmp/sigint_harness" INT; while true; do sleep 1; done` via `podman exec -i -t` on the PTY slave and harness writes `0x03` to the master
- **THEN** `/tmp/sigint_harness` appears

### Requirement: Reply-path DA1
The driver SHALL allow capability queries (`\033[c` DA1, `XTGETTCAP`, Kitty `ESC[?u`) to be sent outbound and replies inbound via the same PTY.

#### Scenario: DA1 round-trip via harness
- **WHEN** container `printf "\033[c"` via `podman exec -i -t` on the PTY slave and harness (playing terminal) writes `\033[?6c` to the master
- **THEN** container `read -d c` receives `?6`

### Requirement: Large-payload integrity
The driver SHALL preserve byte-for-byte integrity for ≥1 MB payloads through stacked PTYs.

#### Scenario: 1 MB sha256
- **WHEN** `head -c 1M /dev/zero | tr '\0' 'A'` is streamed via `podman exec` from container to host
- **THEN** `sha256` inside (`4e29ad18…`) equals host `sha256` and byte count is `1048576`
