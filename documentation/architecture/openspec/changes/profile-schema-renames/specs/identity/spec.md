# identity Specification Delta

## MODIFIED Requirements

### Requirement: Per-identity signing via SSH agent (sign-only is key registration + absence)
The driver SHALL mount a per-identity `AF_UNIX` SSH agent socket read-only when `credential-surface = {ssh_agent="/run/.../seat.sock"}` and SHALL set `SSH_AUTH_SOCK` inside the container; `credential-surface = "none"` mounts nothing. Sign-only SHALL be enforced by GitHub key registration (key type `Signing` refuses auth) plus absence of any auth-registered credential; the socket itself is a normal agent. Identity is `cistella.identity` label (renamed from `cistella.seat`, not credential selector; credential surface remains profile-driven, Phase 2 binds identity to credential, S2) and `cistella.command` label records argv actually executed (renamed from `cistella.harness`).
#### Scenario: Hyphenated credential-surface key parses
- **WHEN** a profile sets `credential-surface = "none"` or `credential-surface = { ssh_agent = "/run/seat.sock" }`
- **THEN** resolution succeeds with the pre-rename spellings failing closed at parse time
