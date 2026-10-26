## ADDED Requirements

### Requirement: Per-poll diagnostic record

Each landing-wait poll SHALL record a diagnostic record containing per-command start/end monotonic offsets and the query order (inspect then exists), the inspect exit/signal/timeout disposition, capped escaped stdout AND stderr, the `podman container exists` exit code for the same container name, and the bound pair under test. Raw StartedAt strings SHALL be kept intact and never reconstructed from whitespace tokens.

#### Scenario: Failed poll keeps both streams

- **WHEN** an inspect exits non-zero with stderr content
- **THEN** the poll record retains the exit code, the capped escaped stdout, and the capped escaped stderr (no stream is discarded)

#### Scenario: Command order and offsets visible

- **WHEN** a poll runs both probes
- **THEN** the record carries per-command start/end monotonic offsets and the query order, so a transition straddled between the two sequential observations is detectable

#### Scenario: Exists code recorded alongside inspect

- **WHEN** a poll runs
- **THEN** the record carries the `container exists` code (0 found, 1 not found, 125 storage/access error) next to the inspect outcome from the same poll iteration

### Requirement: Unqueryable outcomes labeled as unqueryable

Unqueryable poll outcomes (non-zero inspect with no usable identity, exists 125) SHALL be labeled unqueryable in every report and SHALL NEVER be aggregated into an absence finding. The existing bound verdict wording stays unchanged: no wording fix is folded into this instrumentation-only scope.

#### Scenario: No absence finding from query failures

- **WHEN** the wait observes only unqueryable outcomes
- **THEN** the report states the container was not successfully queried (never that it was absent or that surgery never landed)

### Requirement: Pre-surgery baseline snapshot

Before the surgery command runs, the test SHALL capture a bounded baseline: the bound pair (already known at bind time), one inspect plus one exists outcome pair for the container, and one `systemctl --user show` snapshot of the unit. The timeout post-mortem is compared against this baseline, not against assumption.

#### Scenario: Baseline anchors the timeout comparison

- **WHEN** the wait times out
- **THEN** the report carries both the pre-surgery baseline and the timeout snapshot, so unit-failed, clean-stop, and start-lost read as changes from a recorded state

### Requirement: Bounded poll history with first anomaly

The wait loop SHALL retain the first-anomaly time (first poll whose outcome differs from clean-success), outcome counts and transitions, and the first and last N poll records with N small and fixed. It SHALL NOT retain the full 120-poll trace.

#### Scenario: Flap history survives to the bound

- **WHEN** the wait times out after mixed outcomes
- **THEN** the failure report names the first-anomaly time, per-outcome counts, observed transitions, and the bounded first/last records

### Requirement: Bounded subprocess allowances charged to the remaining budget

Every diagnostic subprocess (inspect, exists probe, post-mortem commands) SHALL run under a small fixed allowance, and the total wait SHALL NOT exceed its bound regardless of allowance: the bound on the total is the normative constraint, and any per-probe value satisfying it is acceptable. The chosen value and its rationale SHALL be recorded in a code comment and in the implementation packet; it SHALL NOT be derived from retained suite.log artifacts, which carry no per-poll timings. Timeout-marked evidence SHALL be labeled as timeout, not as a backend outcome.

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

The poll-outcome and post-mortem taxonomy (exists 0/1/125, inspect failure with stderr shapes, unqueryable labeling with no absence aggregation, signal/spawn/timeout dispositions, truncation markers, original-verdict retention against later-healthy observations) SHALL be pinned by `roster-check.py --self-test` fixtures without cargo or backend.

#### Scenario: Taxonomy covered without backend

- **WHEN** the self-test runs
- **THEN** synthetic fixtures exercise every outcome class and every truncation/timeout marker the collector can emit
