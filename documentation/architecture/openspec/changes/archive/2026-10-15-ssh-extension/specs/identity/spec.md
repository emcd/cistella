# identity Specification (delta)

## MODIFIED Requirements

### Requirement: Per-identity signing via SSH agent (sign-only is key registration + absence)

A profile declaring the `ssh` extension SHALL forward the seat's `AF_UNIX` SSH agent socket read-only with `SSH_AUTH_SOCK` set inside the container, contributed by the shipped ssh guest through the central prepare merge from seat ambient; no `credential-surface` slot exists (profiles naming it refuse at parse). Sign-only SHALL be enforced by GitHub key registration (key type `Signing` refuses auth) plus absence of any auth-registered credential; the socket itself is a normal agent. Identity is `cistella.identity` label (renamed from `cistella.seat`, not credential selector; Phase 2 binds identity to credential, S2) and `cistella.command` label records argv actually executed (renamed from `cistella.harness`).
#### Scenario: Credential-surface refuses at parse

- **WHEN** a profile sets `credential-surface = "none"` or `credential-surface = { ssh_agent = "/run/seat.sock" }`
- **THEN** resolution refuses typed at parse; the slot is deleted, migration is to declared `ssh`
