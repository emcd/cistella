## MODIFIED Requirements

### Requirement: Per-identity signing via SSH agent (sign-only is key registration + absence)
The driver SHALL mount a per-identity `AF_UNIX` SSH agent socket read-only when `credential_surface = {ssh_agent="/run/.../seat.sock"}` and SHALL set `SSH_AUTH_SOCK` inside the container; `credential_surface = "none"` mounts nothing. Sign-only SHALL be enforced by GitHub key registration (key type `Signing` refuses auth) plus absence of any auth-registered credential; the socket itself is a normal agent. Identity is `cistella.identity` label (renamed from `cistella.seat`, not credential selector; credential surface remains profile-driven, Phase 2 binds identity to credential, S2) and `cistella.command` label records argv actually executed (renamed from `cistella.harness`).

#### Scenario: Signing works, auth does not
- **WHEN** container runs `ssh-add -l` and `git commit -S -m "review"` with the per-identity key (registered on GitHub as `Signing`)
- **THEN** `ssh-add -l` lists the key and `git commit -S` succeeds, while an `ssh` auth attempt would be refused server-side for that key type and no auth-registered key exists in the container

### Requirement: Allowed signers verification
The driver SHALL provision `allowed_signers` and SHALL verify a signed review commit host-side against the identity's public key.

#### Scenario: Signed review commit
- **WHEN** container creates `git commit -S -m "review"` with the per-identity key
- **THEN** host `ssh-keygen -Y verify -f allowed_signers` succeeds

### Requirement: Credential-absence push enforcement
The driver SHALL NOT provide any Git/GitHub write authentication credential or push-capable IPC inside the container; push SHALL be host-side after signed-commit handoff (requires full egress for the `ssh` probe).

#### Scenario: No push credential
- **WHEN** `env | grep -i github` and `ssh -o BatchMode=yes -T git@github.com` are run inside the container
- **THEN** no `GITHUB_TOKEN` is present and `ssh -o BatchMode=yes` fails without prompting, while `git log --show-signature` shows a valid signature (egress is required for the `ssh` probe to reach `github.com`)
