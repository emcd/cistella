## MODIFIED Requirements

### Requirement: Isolator interface with declared capabilities

Isolators SHALL implement `create` / `initiate` / `execute` / `inspect` / `state` / `terminate` / `remove` as distinct external operations. `inspect` returns a rich read-only snapshot (configuration, detailed status); `state` returns the lifecycle-state enum only (created/initiated/executing/stopped/absent); every state transition remains a separate semantic operation. The internal Rust trait may share an enum-bearing method. Each operation declares preconditions and postconditions, typed resource handles, partial-success reporting, and exit/status delivery. The framework SHALL own lifecycle sequencing and idempotent teardown semantics; isolators SHALL own runtime mechanics. Teardown SHALL be safely repeatable (removing an absent unit succeeds). Concurrent removers converging on the same unit file SHALL neither fail solely because another remover already unlinked: a filesystem `NotFound` on unit-file unlink is converged absence, not an error; every other IO failure still propagates, and a successful unlink still owns `daemon-reload` (reload failure remains an error). This scopes to unit-file removal only, not to all concurrent lifecycle failures. After an uncertain mutating outcome (timeout with unknown applied state), the isolator SHALL support recovery by reconciliation: re-inspect, report actual state, and converge to either applied or clean.

#### Scenario: Idempotent teardown
- **WHEN** terminate/remove runs against an already-removed unit
- **THEN** the isolator reports success with no error and no residue

#### Scenario: Concurrent removers converge on NotFound
- **WHEN** two removers race on the same unit file and the loser observes `NotFound` on unlink
- **THEN** the loser reports converged success with no error and attempts no reload on the absent path; a successful unlink still reloads and reload failure still errors
