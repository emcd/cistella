# identity Specification

## Purpose
TBD - created by archiving change add-cistella-driver. Update Purpose after archive.
## Requirements
### Requirement: Per-identity signing via SSH agent (sign-only is key registration + absence)

The driver SHALL mount a per-identity `AF_UNIX` SSH agent socket read-only when `credential-surface = {ssh_agent="/run/.../seat.sock"}` and SHALL set `SSH_AUTH_SOCK` inside the container; `credential-surface = "none"` mounts nothing. Sign-only SHALL be enforced by GitHub key registration (key type `Signing` refuses auth) plus absence of any auth-registered credential; the socket itself is a normal agent. Identity is `cistella.identity` label (renamed from `cistella.seat`, not credential selector; credential surface remains profile-driven, Phase 2 binds identity to credential, S2) and `cistella.command` label records argv actually executed (renamed from `cistella.harness`).
#### Scenario: Hyphenated credential-surface key parses
- **WHEN** a profile sets `credential-surface = "none"` or `credential-surface = { ssh_agent = "/run/seat.sock" }`
- **THEN** resolution succeeds with the pre-rename spellings failing closed at parse time

### Requirement: Allowed signers verification
The driver SHALL provision `allowed_signers` and SHALL verify a signed review commit host-side against the seat's public key.

#### Scenario: Signed review commit
- **WHEN** container creates `git commit -S -m "review"` with the per-seat key
- **THEN** host `ssh-keygen -Y verify -f allowed_signers` succeeds

### Requirement: Credential-absence push enforcement
The driver SHALL NOT inject any Git/GitHub write authentication credential or push-capable IPC inside the container via credential-surface, assignments, or templates; push SHALL be host-side after signed-commit handoff (requires full egress for the `ssh` probe). Explicit `environment-acceptances` are an operator override to this absence: an exactly-named variable — including a push-capable one — is forwarded verbatim because the operator named it deliberately. Diagnostics display names only; accepted values are stored in unit `Environment=` lines and visible to principals able to read or inspect them.

#### Scenario: No push credential
- **WHEN** `env | grep -i github` and `ssh -o BatchMode=yes -T git@github.com` are run inside the container of a profile with no push-capable `environment-acceptances`
- **THEN** no `GITHUB_TOKEN` is present and `ssh -o BatchMode=yes` fails without prompting, while `git log --show-signature` shows a valid signature (egress is required for the `ssh` probe to reach `github.com`)

#### Scenario: Explicit acceptance overrides credential absence
- **WHEN** a profile lists a push-capable name (e.g. `GITHUB_TOKEN`) in `environment-acceptances` with the variable present in the invoker environment
- **THEN** conduct forwards it verbatim with name-only diagnostics, while the assignment, template, and credential-surface paths still refuse to inject it
