## ADDED Requirements

### Requirement: Dispatcher linearization with unified post-terminal error

Admitted SHALL mean present in the dispatcher's `pending` table (inserted only after guest-send success on the single dispatcher thread); mpsc-send success alone SHALL NOT constitute admission. The terminal transition SHALL be `fatal_terminal` draining pending with the composed reason (or orderly Shutdown), `stopping` set, and thread exit; "first terminal" SHALL mean the first fatal-exchange terminal — ordinary per-request ok/error replies SHALL NEVER mark dispatcher terminal. Ops issued after the first fatal terminal is received are DETERMINISTICALLY post-terminal and SHALL fail with ONE documented typed post-terminal error (`CistellaError::Protocol`), replacing the incidental pair. Queued-but-unadmitted ops at transition are post-terminal, not a third case. Ops admitted before terminal SHALL receive their genuine terminal. Uncertainty and death dominance keep their contract: failed shutdown proof sets shutdown-uncertain without latching death; the uncertain path never returns clean.

#### Scenario: Admitted op names the shutdown failure

- **WHEN** an op is admitted before dispatcher terminal and the fatal path's shutdown proof fails
- **THEN** the op fails naming the shutdown failure, the client sets shutdown-uncertain without latching death, and teardown converges by name without ever returning clean

#### Scenario: Post-terminal op gets the unified error

- **WHEN** an op is issued after the first fatal-exchange terminal was received (send-refused or orphaned-disconnect alike)
- **THEN** it fails with the single documented post-terminal `Protocol` error, never with an admitted-path terminal

#### Scenario: Ordinary replies never mark terminal

- **WHEN** ops complete with ordinary ok or error replies while the exchange lives
- **THEN** later ops are unaffected; only a fatal-exchange terminal transitions the dispatcher

#### Scenario: Post-terminal error preserves dominance

- **WHEN** a post-terminal error passes through `death_checked` while the uncertain flag is set
- **THEN** it returns the RECORDED shutdown failure via `shutdown_dominant` (uncertainty/residue dominance preserved, not bypassed)
