## MODIFIED Requirements

### Requirement: Transport uses host PTY with podman exec -i -t on PTY slave via conduct/enter
The driver SHALL run `podman exec -i -t` with stdio on the session PTY slave (tmux pane or Agentmux Pty slave) and SHALL never use `podman attach` to PID 1. `conduct` SHALL own the harness lifetime through the framework lifecycle with Podman as the implementing isolator: create unit -> initiate -> `podman exec -i -t` harness argv after `--` (or profile `command` array) on the pane PTY -> wait -> shared teardown with reverse-order cleanup, exiting with harness status or `128+signal` after `SIGHUP`/`SIGTERM`. Every phase is contract-bound and conformance-pinned (see the isolator-contract capability); transport behavior is otherwise unchanged. `enter` SHALL provide companion shell entry using the same transport, with exactly one selector per invocation — positional `<id>` unique prefix, or `--directory <path>`, or one or more `--label k=v` (ANDed, `cistella.` refused, canonicalized), mixing forms is usage error and zero/multiple matches are typed refusals listing candidates.

#### Scenario: Piped exec is deaf
- **WHEN** `podman exec` is run with piped stdio
- **THEN** `isatty` is false and `stty size` is `0 0` and inbound `DA1`/`0x03` are deaf

#### Scenario: PTY slave exec is live
- **WHEN** `podman exec -i -t` is run with stdio on the pane PTY slave
- **THEN** `isatty` is true, `stty size` reflects the host window, and `DA1`/`0x03` traverse

#### Scenario: Selector ambiguous is typed refusal
- **WHEN** `enter --directory .` matches two sessions on same directory
- **THEN** driver refuses with typed error listing candidates (I2)

#### Scenario: Transport phases are contract-bound
- **WHEN** the Podman isolator executes create/initiate/execute/terminate/remove
- **THEN** each phase satisfies the isolator contract and the conformance suite pins the behavior end to end
