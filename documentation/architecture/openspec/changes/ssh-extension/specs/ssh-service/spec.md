# ssh-service Specification

## Purpose
Agent-socket forwarding provided by a standalone shipped extension: declaration-gated, ambient-sourced, exact-tuple contribution. Synced from change ssh-extension.

## ADDED Requirements

### Requirement: SSH agent forwarding via declared extension

A profile declaring `[[extensions]]` `ssh` SHALL conduct the shipped ssh guest (sibling `cistella-extension-ssh` binary, install-dir trust like the tmpdir sibling — no digest verification). The driver SHALL read invoker `SSH_AUTH_SOCK` at selection and spawn `--socket=P --socket-canonical=C` (P ambient value, C fs-canonicalized, both host-supplied); the guest's only filesystem read SHALL be the `S_ISSOCK` kind check on C. Ambient alone SHALL never inject — without declared `ssh`, no agent moves. Declared `ssh` with unset, missing, or non-socket ambient SHALL refuse pre-create typed (declared-but-unprovidable). The guest's hello role SHALL be `ssh`, advertising `environment`, `mounts`, `permissions`, `credentials`. The instance SHALL return exactly `{env: (SSH_AUTH_SOCK, P), mount: (C → P, RO), permission: (P, read), handle: (C)}` with no extras, hooks, or policy claims; omission, divergence, extras, or empty env SHALL refuse whole. The `RO` bind is mount hygiene, not an agent-protocol constraint. Registry entries named `ssh` SHALL refuse at parse. No `credential-surface` slot exists — profiles naming it refuse at parse.

#### Scenario: Declared ssh forwards the seat agent

- **WHEN** a profile declares `[[extensions]]` `ssh` with invoker `SSH_AUTH_SOCK=P` naming a live socket
- **THEN** the harness observes `SSH_AUTH_SOCK=P` with the socket mounted read-only at P from host source C, contributed through the central merge in declaration order

#### Scenario: Undeclared means no agent

- **WHEN** a profile declares no `ssh` and neither accepts nor assigns `SSH_AUTH_SOCK`
- **THEN** no ssh guest is spawned and the container observes no `SSH_AUTH_SOCK` (ambient never injects; explicit acceptance is a separate verbatim override)

#### Scenario: Declared-but-unprovidable refuses

- **WHEN** a profile declares `ssh` with ambient unset, missing, or naming a non-socket
- **THEN** conduct refuses pre-create typed; no session runs agentless on a declared agent

#### Scenario: Real P!=C bundle merges and forwards

- **WHEN** ambient P is a symlink to a socket at canonical C and the instance returns the exact tuple
- **THEN** the merge admits it (placement P matches permission P) and the harness observes the forwarded agent at P

#### Scenario: Host-canonical permission refuses

- **WHEN** the instance returns permission target C instead of P
- **THEN** the merge refuses typed (permission C orphans against placement P)

#### Scenario: Divergent or empty bundle refuses whole

- **WHEN** the instance returns a handle for P with a mount for Q, a wrong env value, a missing handle or mount, or empty env
- **THEN** the merge refuses the whole transaction typed; partial ssh plans never conduct

#### Scenario: Registered ssh refuses at parse

- **WHEN** a registry file names an entry `ssh`
- **THEN** registration refuses typed (shadowing refusal); shipped-first selection never meets it

#### Scenario: Credential-surface refuses at parse

- **WHEN** a profile names `credential-surface` in any form
- **THEN** profile load refuses typed; the slot is gone, migration is to declared `ssh`

#### Scenario: Slot owned under declaration, reserved otherwise

- **WHEN** a declared-ssh session meets another `SSH_AUTH_SOCK` claimant, or any session without declared ssh meets a guest contributing it
- **THEN** the other claimant refuses (owned name under declaration, reservation otherwise); the ssh value never silently wins or loses

#### Scenario: Acceptance without declaration forwards verbatim

- **WHEN** a profile without declared `ssh` lists `SSH_AUTH_SOCK` in `environment-acceptances` with ambient set
- **THEN** conduct forwards it verbatim with name-only diagnostics (explicit operator override); with declared `ssh` the acceptance refuses (owned name)
