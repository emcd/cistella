# surgery-diagnostics Specification

## Purpose

Test-only diagnostic evidence for post-surgery container query failures: bounded per-poll records, pre-surgery baseline, and timeout post-mortem beside the unchanged verdict.

## Requirements

### Requirement: Per-poll diagnostic record

Each landing-wait poll SHALL record a diagnostic record containing per-command start/end monotonic offsets and the query order (inspect then exists), the inspect exit/signal/spawn/timeout disposition, capped escaped stdout AND stderr, the `podman container exists` exit code for the same container name, and the bound pair under test. Raw StartedAt strings SHALL be kept intact and never reconstructed from whitespace tokens. Stderr strings are retained as-is; no classification SHALL be derived from message prose — code, signal, spawn, and timeout are the primary facts, with a generic query-failure category when the cause is unknown.

#### Scenario: Failed poll keeps both streams

- **WHEN** an inspect exits non-zero with stderr content
- **THEN** the poll record retains the exit code, the capped escaped stdout, and the capped escaped stderr verbatim (no stream is discarded, no prose is diagnosed)

#### Scenario: Exists code recorded alongside inspect

- **WHEN** a poll runs
- **THEN** the record carries the `container exists` code (0 found, 1 not found, 125 storage/access query failure per CLI contract) next to the inspect outcome from the same poll iteration

#### Scenario: Command order and offsets visible

- **WHEN** a poll runs both probes
- **THEN** the record carries per-command start/end monotonic offsets and the query order, so a transition straddled between the two sequential observations can be recognized when the two observations differ (sequential queries alone cannot prove every transition)

### Requirement: Unqueryable outcomes labeled as unqueryable

A non-zero inspect with no usable identity SHALL be labeled query failure; exists 125 SHALL be labeled storage/access query failure; exists 1 SHALL support absence only at that observation, never continuous absence. Unqueryable outcomes SHALL NEVER be aggregated into an absence finding. The report SHALL carry a diagnostic evidence section stating the container was not successfully queried; that section is rendered with the bound verdict — adjacent to it, or ahead of it — so the retained verdict wording is not read as proven absence. The verdict wording itself stays exactly as it is: no wording fix is folded into this instrumentation-only scope.

#### Scenario: No absence finding from query failures

- **WHEN** the wait observes only unqueryable outcomes
- **THEN** the diagnostic evidence section states the container was not successfully queried (never that it was absent), rendered alongside the unchanged verdict text

### Requirement: Pre-surgery baseline snapshot

Before the surgery command runs, the test SHALL capture a bounded baseline under its own small fixed allowance: the bound pair (already known at bind time), one inspect plus one exists outcome pair for the container, and one `systemctl --user show` snapshot of the unit. A baseline that already shows an inactive unit or failed queries is recorded as an anomalous premise, not as evidence the setup was healthy. No baseline success assertions and no new test-behavior gates are added in this scope; baseline failure or unavailability is recorded and the original setup assumptions stay visible.

#### Scenario: Baseline anchors the timeout comparison

- **WHEN** the wait times out
- **THEN** the report carries both the pre-surgery baseline and the timeout snapshot, so unit-failed, clean-stop, and start-lost read as changes from a recorded state with stated limits

### Requirement: Bounded poll history with first anomaly

The wait loop SHALL retain the first-anomaly time (first poll whose outcome differs from clean success), per-outcome counts and transitions, and the first and last N poll records with N small and fixed. It SHALL NOT retain the full 120-poll trace.

#### Scenario: Flap history survives to the bound

- **WHEN** the wait times out after mixed outcomes
- **THEN** the failure report names the first-anomaly time, per-outcome counts and transitions, and the bounded first/last records

### Requirement: Phased bounded budget

The diagnostic work SHALL be composed of explicit phases: a bounded pre-surgery baseline allowance B, the unchanged 60s landing deadline W, then a separately bounded failure-report allowance D that starts only after the original timeout is latched, plus accounted kill/reap/drain cleanup. Inside W, every diagnostic subprocess (inspect, exists probe) SHALL run under a small fixed per-probe allowance, so no single probe can consume a fresh window inside the deadline. No new polling SHALL start after W. Collection is exactly-once attempt, not guaranteed bundle: missing, unscheduled, or timed-out diagnostics are reported as such, and timeout-marked evidence SHALL be labeled as timeout, never counted as a backend outcome. The stated end-to-end bound is B+W+D+cleanup; a per-command timeout alone does not compose it.

#### Scenario: Post-timeout work fits its own allowance

- **WHEN** the landing timeout fires and the post-mortem commands cannot all start or complete inside D
- **THEN** each missing, unscheduled, or timed-out diagnostic is reported as such and the original timeout remains the verdict

#### Scenario: Late changed identity is rejected, not landed

- **WHEN** a changed pairing is observed at or past the W deadline
- **THEN** the observation is recorded with a late marker and does not count as a landing; the wait times out instead

### Requirement: One heavy post-mortem on the timeout path

On the timeout path only, inside allowance D and before Guard cleanup, the collector SHALL capture exactly once: `systemctl --user show` machine-readable unit properties (LoadState, ActiveState, SubState, Result, ExecMainCode, ExecMainStatus, NRestarts, InvocationID, StartLimit settings, actual Restart/SuccessExitStatus, monotonic state-change times, main/control PIDs), `podman ps -a` filtered to the test container name with the known bound container ID compared against the listing in Rust (match recorded; absence of a match recorded as such), and a bounded unit journal slice from the recorded pre-surgery walltime/cursor. The collector SHALL perform no reset-failed, restart, relaunch, prune, gc, lock acquisition, or serialization. Snapshot properties and journal slices support hypotheses; they do not by themselves prove which actor removed a container, and any remaining alternatives are recorded as unresolved with limits.

#### Scenario: Post-mortem fields support comparison with stated limits

- **WHEN** the timeout post-mortem runs
- **THEN** the bundle contains the unit, process, and journal fields needed for useful comparison, and any candidate mechanism the evidence cannot separate is recorded as unresolved rather than assigned

#### Scenario: Collector mutates nothing

- **WHEN** the post-mortem runs
- **THEN** no reset, restart, relaunch, prune, gc, lock, or serialization command is issued by the collector

### Requirement: Original failure retained

If any post-mortem query succeeds after the verdict failed, the report SHALL lead with the original failure directly adjacent to a prominently labeled unqueryable note (absence not established); post-mortem success SHALL NOT replace or soften it.

#### Scenario: Post-mortem success does not exonerate

- **WHEN** the timeout fired but a later diagnostic query observes a healthy container
- **THEN** the report retains the timeout as the verdict with the later observation appended as subsequent state

### Requirement: Landing history retained for later failures

The successful mutation's identity, bounded landing history, and the pre-surgery baseline SHALL be dumped alongside any failure that occurs after landing succeeded (including the separate exists-but-uninspectable admittability class), without changing any verdict semantics. The mutation identity names the argv as run, the successful exit status, and both streams capped at 4000 kept characters each with explicit truncation markers naming the dropped character count; the section is labelled rendered-and-not-re-runnable. The landing-history collection is scoped to landing-timeout evidence; this proposal extends the retained set on the successful-landing path, and later-failure capture reuses the retained set only, and the gap (no dedicated post-mortem on the later path) is disclosed, not closed by scope expansion.

#### Scenario: Later failure keeps its context

- **WHEN** a post-landing failure occurs
- **THEN** the report appends the retained mutation identity, bounded landing history, and baseline with verdict semantics unchanged

#### Scenario: Mutation identity is bounded and marked

- **WHEN** the retained mutation produced more than 4000 characters on a stream
- **THEN** the rendered section carries the head of the stream plus an explicit truncation marker naming the dropped character count, never the full unbounded stream

### Requirement: Collector classifications pinned through the actual Rust implementation

The poll-outcome, history-cap, original-verdict-retention, and budget-composition classifications SHALL be pinned by tests invoking the same Rust record/classify/report/history functions the collector uses, in a backend-free test module beside the collector. No independent Python model of Rust behavior SHALL stand as product-path evidence; the quarantine checker stays scoped to quarantine.

#### Scenario: Pins execute the real seam

- **WHEN** the backend-free collector tests run
- **THEN** they exercise the actual record, classification, history-cap, retention, and budget functions through the same implementation the test path calls, covering exists codes, stderr shapes, unqueryable labeling with no absence aggregation, signal/spawn/timeout dispositions, truncation markers, and original-verdict retention
