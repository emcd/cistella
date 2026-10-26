## ADDED Requirements

### Requirement: Per-poll diagnostic record

Each landing-wait poll SHALL record a diagnostic record containing the inspect exit/signal/timeout disposition, capped escaped stdout AND stderr, the `podman container exists` exit code for the same container name, a monotonic timestamp, and the bound pair under test. Raw StartedAt strings SHALL be kept intact and never reconstructed from whitespace tokens.

#### Scenario: Failed poll keeps both streams

- **WHEN** an inspect exits non-zero with stderr content
- **THEN** the poll record retains the exit code, the capped escaped stdout, and the capped escaped stderr (no stream is discarded)

#### Scenario: Exists code recorded alongside inspect

- **WHEN** a poll runs
- **THEN** the record carries the `container exists` code (0 found, 1 not found, 125 storage/access error) next to the inspect outcome from the same poll iteration

### Requirement: Bounded poll history with first anomaly

The wait loop SHALL retain the first-anomaly time (first poll whose outcome differs from clean-success), outcome counts and transitions, and the first and last N poll records with N small and fixed. It SHALL NOT retain the full 120-poll trace.

#### Scenario: Flap history survives to the bound

- **WHEN** the wait times out after mixed outcomes
- **THEN** the failure report names the first-anomaly time, per-outcome counts, observed transitions, and the bounded first/last records

### Requirement: Bounded subprocess allowances charged to the remaining budget

Every diagnostic subprocess (inspect, exists probe, post-mortem commands) SHALL run under a small fixed allowance deducted from the wait's remaining deadline, such that the total wait cannot exceed its bound. Timeout-marked evidence SHALL be labeled as timeout, not as a backend outcome.

#### Scenario: Slow probe cannot extend the wait

- **WHEN** a probe exceeds its allowance
- **THEN** it is abandoned, marked timeout, and the outer deadline is unchanged

### Requirement: One heavy post-mortem on the timeout path

On the timeout path only, and before Guard cleanup, the collector SHALL capture exactly once: `systemctl --user show` machine-readable unit properties (LoadState, ActiveState, SubState, Result, ExecMainCode, ExecMainStatus, NRestarts, InvocationID, StartLimit settings), `podman ps -a` filtered to the test container name, and a bounded unit journal slice. The collector SHALL perform no reset-failed, restart, relaunch, prune, gc, lock acquisition, or serialization.

#### Scenario: Unit-failed versus clean-stop is distinguishable

- **WHEN** the timeout post-mortem runs
- **THEN** the bundle contains Result, NRestarts, and start-limit evidence sufficient to separate unit-failed/rate-limited from clean accepted stop from unit-active-with-missing-container

#### Scenario: Collector mutates nothing

- **WHEN** the post-mortem runs
- **THEN** no reset, restart, relaunch, prune, gc, lock, or serialization command is issued by the collector

### Requirement: Original failure retained

If any post-mortem query succeeds after the verdict failed, the report SHALL lead with the original failure; post-mortem success SHALL NOT replace or soften it.

#### Scenario: Post-mortem success does not exonerate

- **WHEN** the timeout fired but a later diagnostic query observes a healthy container
- **THEN** the report retains the timeout as the verdict with the later observation appended as subsequent state

### Requirement: Backend-free classification pins

The poll-outcome and post-mortem taxonomy (exists 0/1/125, inspect failure with stderr shapes, signal/spawn/timeout dispositions, truncation markers, timeout-labeled evidence) SHALL be pinned by `roster-check.py --self-test` fixtures without cargo or backend.

#### Scenario: Taxonomy covered without backend

- **WHEN** the self-test runs
- **THEN** synthetic fixtures exercise every outcome class and every truncation/timeout marker the collector can emit
