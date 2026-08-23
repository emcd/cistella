## ADDED Requirements

### Requirement: Rootless Podman lifecycle with labels and GC (no --rm)
The driver SHALL manage one disposable container per session with labels `cistella.session-id`, `cistella.seat`, `cistella.harness`, `cistella.profile` and SHALL provide `stop`/`status`/`logs`/`gc` verbs. For the spike: `podman run -d --userns=keep-id`; for the V1 driver: `run` generates a Quadlet `.container` unit and `systemctl --user start` it (systemd owns the container). The driver SHALL NOT use `--rm` — removal is `gc`'s job. `gc` SHALL reap only `exited` containers with `cistella.*` labels in V1; running-orphan detection is deferred. `gc` SHALL fail closed (reap nothing) on any `podman inspect` error.

#### Scenario: Label and GC
- **WHEN** a Quadlet unit `cistella-*.container` is generated and started and the session crashes
- **THEN** `podman ps --filter label=cistella.session-id=...` finds the exited container and `cistella gc` reaps it and `cistella logs` shows post-mortem

#### Scenario: GC isolation (never reap unrelated or active)
- **WHEN** an unrelated container without `cistella.*` labels, an active `cistella` session container (running with `cistella.*` labels), and an exited `cistella` session container exist
- **THEN** `cistella gc` reaps only the exited `cistella` container whose `podman inspect --format '{{.State.Status}}'` is `exited` and leaves the unrelated and active `running` containers untouched; on any `podman inspect` error `gc` SHALL fail closed and reap nothing (running-orphan reaping is deferred to a later change)

### Requirement: Quadlet supervision
The driver SHALL use Quadlet systemd user units for long-lived supervision, not `podman generate systemd`.

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
