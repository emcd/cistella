## ADDED Requirements

### Requirement: Durable per-operation log layout

Each mutating lifecycle operation SHALL append records under `XDG_STATE_HOME/cistella/logs/<operation-id>/` (fallback `~/.local/state/cistella/logs/` when XDG_STATE_HOME is unset). Directories SHALL be created anchored with mode 0700 and files with mode 0600, following the repo's existing secure-path discipline (no symlink follow, uid-keyed ownership). Missing ancestors of the log root SHALL be created with the same discipline (verified, not hardened); a symlink at any level refuses the open. A fresh seat with no state chain MUST still open its first operation log. New logs SHALL NOT live inside `cistella-state-<uid>` and SHALL NOT mirror the lease layout. The runtime directory MAY stage bounded coordination metadata only; any runtime-only retention SHALL name its weaker lifetime and require export before teardown.

#### Scenario: Operation opens its own directory

- **WHEN** a lifecycle operation starts
- **THEN** it creates exactly one `<operation-id>` directory with the required modes, and all of that operation's records land inside it

#### Scenario: No cohabitation with existing state trees

- **WHEN** a reviewer inspects the log tree
- **THEN** no log path sits under `cistella-state-<uid>` and no lease-layout directory is created for logging

### Requirement: Operation envelope with start and terminal records

Every operation log SHALL open with a start record carrying the operation id, parent operation id (when nested), actor verb plus a role from the fixed vocabulary (`orphan-reaping`, `installing`, `starting`, `terminating`, `tearing-down`, `unknown`), pid plus process start identity, argv label from the fixed operation vocabulary, and the start wall-clock timestamp. Every operation SHALL append a terminal record carrying the end wall-clock timestamp, the monotonic duration, and the outcome, or else the operation reads as incomplete. Records in one operation file SHALL carry sequence numbers starting at zero. Minted operation ids SHALL use a path-safe alphabet by construction (lowercase alphanumerics, fixed length). Unknown verbs SHALL be recorded under the fixed `unknown` label and role, never verbatim, in every build profile.

#### Scenario: Complete operation reads open to close

- **WHEN** an operation finishes
- **THEN** its log holds a start record, numbered body records, and a terminal record with the outcome

### Requirement: Length-prefixed framing with a stated reader contract

Records SHALL be stored as length-prefixed frames (4-byte big-endian length plus JSON bytes, maximum frame payload 256 KiB), not bare lines. The reader SHALL check the declared length against the maximum payload and the remaining file bytes BEFORE any allocation. A partial prefix (1-3 bytes), a short payload, invalid JSON, an invalid schema, or a sequence gap SHALL discard that frame and every byte after it in that file, preserve the complete valid prefix before it, and record the discard. Records form a state machine: a start record first and only first, body records while open, at most one terminal record last; any out-of-order, duplicated, or unknown-kind record, or any record missing its sequence or kind, ends the valid prefix at that frame. A missing start record or a missing terminal record each has explicit handling: the stream reads UNKNOWN, and a terminal outcome SHALL NEVER be accepted through unchecked corruption or a sequence gap. Reads SHALL be byte-bounded independent of any metadata pre-check: a file larger than the bound is refused, never partially read. Control-file reads SHALL open non-blocking with symlink-following refused and read only verified regular files, so a planted FIFO or a replaced path cannot stall or over-allocate the reader; a non-regular or over-bound file is refused, never partially read. The reader's discard accounting SHALL live in its derived result or separate evidence, never by rewriting or repairing the append-only raw log.

#### Scenario: Torn tail is discarded and counted

- **WHEN** the last frame of a log is shorter than its length prefix declares
- **THEN** the reader drops that frame, keeps every complete frame before it, and records that one frame was discarded

#### Scenario: Missing terminal means UNKNOWN

- **WHEN** a log holds a start record and body records but no terminal record
- **THEN** the operation's outcome reads as UNKNOWN, and no absence or success is concluded from it

### Requirement: GC per-target decision records

The gc operation SHALL record, per candidate session: the session id, the observed container-exists disposition (`present`, `absent`, or `query-failed`), the inspect outcome and stderr, the observed status, the unit ActiveState and LoadState as observed (a value), `unavailable` when queried but missing, or `not-queried` when the unit was never read, the decision with a reason code, and the action taken with its result or errno. Fields the scan never reads carry explicit markers: the invocation id, the reconciliation attempt, and the reconciliation key are `not-queried`, and the full container id is `unavailable`. A scan that aborts before classification SHALL still record a scan-level receipt naming the decision (`aborted-ps-failed`, `aborted-query-failed`, `aborted-fail-closed`) with its error; a failed exists query carries the actual query error rather than the unrelated inspect stderr. Nested teardown calls SHALL carry the gc operation id as their parent. The records SHALL cover phase-1 classification (the `to_reap` and live sets), phase-2 teardown plus container removal per classified orphan, and the tmpdir and attestation phases. A final reaped list alone SHALL NOT satisfy this requirement.

#### Scenario: Later reader reconstructs a reap

- **WHEN** a unit file is gone and gc ran in the window
- **THEN** the gc operation log shows whether that session was classified, under which reason code, and what each teardown step returned

### Requirement: Lifecycle operation records

Conduct install/start, terminate, and shared teardown SHALL emit the same envelope and framing with operation-specific observed and decided fields (unit installed or adopted, start outcome, stop outcome, unlink outcome with reload result including unlink-then-reload-failure as its own receipt rather than silence; the unlink result comes from the removing call's own return, never from a pre-check that a concurrent remover could invalidate). This change covers gc first and these operations sequenced after it, in the same change, by explicit operator scope expansion.

#### Scenario: Terminate leaves a receipt

- **WHEN** terminate converges a session
- **THEN** its operation log records the stop, unlink, reload, and attestation outcomes with the same field discipline as gc records

### Requirement: Record only what the decision observed

A record SHALL carry exactly the observations the deciding code path already made, with explicit `not-queried` or `unavailable` markers for fields the path did not read. The logger SHALL NOT issue additional backend queries to fill log fields and SHALL NOT re-evaluate the decision after the fact. Any extra observation beyond the decision's own reads is a separate bounded optional probe, not part of the decision record.

#### Scenario: Missing InvocationID is marked, not fetched

- **WHEN** a gc classification did not read a unit InvocationID
- **THEN** the record carries `not-queried` for that field rather than triggering a new query

### Requirement: Injected test metadata is validated claims

Test-run id, exact SHA, test name, and parent operation id SHALL arrive only by explicit env injection (`CISTELLA_TEST_RUN_ID`, `CISTELLA_SHA`, `CISTELLA_TEST_NAME`, `CISTELLA_PARENT_OP_ID`, each bounded to 128 chars; values beyond the bound are truncated with a marker or the record notes the rejection). The envelope's argv label SHALL come from a fixed operation vocabulary, never a rendered command line. An unknown verb SHALL be recorded under the fixed `unknown` label, never verbatim, in every build profile. Injected values SHALL be recorded as claims and SHALL NEVER be presented as proof of origin. Build provenance SHALL be limited to what exists at compile time and labeled as such.

#### Scenario: Oversized injection is bounded

- **WHEN** an injected value exceeds its bound
- **THEN** the record carries the truncation marker or rejection note, and no path, label, or lookup is built from the unbounded value

#### Scenario: Labels derive from validated ids only

- **WHEN** a record names a session, unit, or path
- **THEN** those values derive from validated session ids and unit names, never from unconstrained test names or path strings

### Requirement: Bounded total retention with admission control and a reserved failure budget

The whole log tree SHALL fit a hard logical budget of 3 GiB of operation bytes. Capacity SHALL be accounted and reserved BEFORE creation: each open writes an owner-attributed reservation file before creating its operation directory, reserving one metadata share plus one full per-operation cap, and reservations are released on close and on drop, including when construction fails after the reservation is written, with dead owners reaped on the next open. An explicit operation id MUST satisfy the path-safe alphabet or the open is refused before any reservation. The reservation covers operation bytes, failure and incomplete operations, active operations, and the eviction-accounting records themselves, with bounded coordination under a nonblocking admission lock (contention skips logging). Reservation reads are bounded, non-blocking, and verified regular files: an unreadable, overlarge, or invalid reservation entry is charged one full reservation share rather than zero, a listing failure refuses the open rather than admitting blind, and non-regular files hold no capacity. Empty and abandoned directories count as metadata against the budget. The physical filesystem may transiently overshoot the logical budget by reservation metadata plus at most one in-flight frame per active writer, and those bounds are the only permitted excess. When the budget cannot be secured, the open is refused and the business operation proceeds without logging; the loss is an observability loss recorded where possible. Post-close eviction alone SHALL NOT be claimed to enforce the cap. The per-operation record count is implied by the per-operation cap and the minimum frame size (4-byte prefix plus at least 2 payload bytes), not separately bounded.

Inside the tree, a reserved failure budget of 1 GiB is protected: success-outcome operations are pruned by count (most recent 2000) and by age (7 days, whichever is smaller) and their pruning SHALL NOT consume the reserve; failure-outcome and incomplete operations are exempt from count pruning, bounded by the 1 GiB reserve and the 7-day age bound. When the reserve is exhausted, the oldest failure operations are evicted first-in-first-out with one accounting record per eviction run, so the loss is observable rather than silent. Per-operation directories remain capped at 1 MiB each. Records SHALL carry no secrets and no full ambient environment.

#### Scenario: Old operations age out

- **WHEN** retention limits are exceeded on close
- **THEN** the oldest operation directories are removed best-effort, and a prune failure is silent (never fails the operation)

#### Scenario: Failed operations survive count pruning within their reserve

- **WHEN** the success count bound is reached while failure-outcome operations are retained
- **THEN** those operations are kept until the reserve or the age bound forces eviction, and eviction leaves an accounting record

#### Scenario: Reserve exhaustion is observable

- **WHEN** the failure reserve is full and a new failure operation closes
- **THEN** the oldest failure operation is evicted first-in-first-out and the eviction is recorded, never silent

### Requirement: Incomplete operations are bounded UNKNOWN evidence against the failure reserve

An operation directory with a start record but no terminal record is an incomplete operation: its outcome is UNKNOWN — it may be a crash, an interruption, a logging failure, cap exhaustion, or process death, and no missing terminal SHALL be asserted as a proven crash. Incomplete operations SHALL count against the failure reserve and age out by the 7-day bound. Active writer ownership is defined by the envelope's pid plus process start identity: an operation is active only while a live process matches both, checked without blocking and without relying on PID reuse or mtime guessing as proof. An operation that is neither active nor terminally closed is inactive-incomplete and IS an eligible prune victim under the reserve and age policy, so abandoned empty directories are never immortal. Logging coordination SHALL NOT wait unboundedly and SHALL NOT change the existing lifecycle lock order: pruning acquires no lifecycle locks and operates only on the log tree.

#### Scenario: Incomplete operation never reads as success or proven crash

- **WHEN** a log holds no terminal record
- **THEN** every consumer reports the outcome as UNKNOWN (crash, interruption, logging failure, cap exhaustion, or process death — never asserted as one of them) and the operation counts against the failure reserve until aged out

### Requirement: Prune safety on shared durable state

Pruning SHALL apply the anchored discipline to deletion: only directories whose names match the minted path-safe alphabet under the log root are eligible; every candidate is lstat-checked with no symlink follow and with type and ownership checks before removal. An operation that carries a finalizing marker AND whose owner is provably live is never a victim: its terminal write or fsync may still be pending. A marker with a dead owner, or with no owner information, falls through to the normal rules, so stale markers never immortalize anything. Victim selection otherwise runs over completed AND inactive-incomplete operations (active ownership defined above); selection and eviction are separate steps so the unlink-time recheck in eviction can skip a victim that grew a live finalizing marker after selection. Logger-owned control and evidence files (the admission lock, eviction markers) SHALL be created and opened nofollow with regular-file verification: a planted symlink or FIFO fails the operation closed without touching anything outside the log tree. The pruner SHALL distinguish active from completed-or-inactive-incomplete before selection, SHALL re-verify the marker and liveness under the admission lock immediately before each unlink, SHALL skip (counted, not failed) when the lock is contended or the recheck finds protection, and SHALL bound its scan work. A write that fails on a prune race is an observability loss only, never a decision change.

#### Scenario: Active operation survives pruning

- **WHEN** a prune run meets an operation still being written
- **THEN** that operation is skipped, completed victims are removed within the bounded scan, and no in-flight decision is affected

### Requirement: Logging never changes decision semantics

A logging write failure, partial record, or concurrent-writer interleave SHALL be an observability loss only: it SHALL NOT change a reap decision, a safety outcome, a failure propagation, or fail-closed behavior. Each operation file SHALL have a single owning writer; concurrent operations SHALL use separate files, and no multi-writer joins SHALL occur. The finalization window is declared: an operation is not eligible for unlinking while its terminal write, its fsync, or any promised per-operation finalization write is still pending, and becomes eligible from the fsync onward, including in its own maintenance pass. Close order is marker creation, terminal write, fsync, marker rename to finalized evidence, then maintenance: no writes to that stream occur after the fsync, so eligibility after it is sound. The terminal records whether the admission lock was held during finalization (`locked`); when the lock cannot be secured the terminal write is skipped and the operation stays explicitly incomplete rather than finalizing unprotected. The finalized marker is finalization evidence, never an in-progress hold: the pruner keys only on the in-progress marker. A single shared outcome vocabulary governs the reserve: only a terminal record reading exactly `success` counts as success; every other completed outcome (including the `failure` that a scoped error closes with) accrues to the protected failure class rather than silently passing as success. Logging SHALL sit beside each decision, after it; gc's inspect-failure abort path SHALL remain untouched by logging code. When the writer detects a partial write it SHALL stop that stream and mark the evidence loss (a best-effort terminal record with outcome UNKNOWN where writable) rather than appending new frames behind an unrecoverable torn frame and pretending the file is complete. New input/output, pruning, and fsync change timing, so the promise is unchanged decision semantics with bounded disclosed observer perturbation (one append per decision point, fsync on close only, prune best-effort off the hot path and acquiring no lifecycle locks) — never zero runtime effect and never "negligible" as fact.

#### Scenario: Logger failure keeps gc semantics

- **WHEN** the log write fails mid-gc
- **THEN** gc completes with exactly the decisions and failure propagation it would have had without logging, and the loss is noted where possible

#### Scenario: Fail-closed path stays logging-free in effect

- **WHEN** a phase-1 inspect failure aborts gc before any teardown
- **THEN** zero teardowns occur, identical to the behavior without logging

### Requirement: Raw logs separate from excerpts

The log file SHALL be append-only and never rewritten. Any excerpt attached to a failure report SHALL be a derived rendering with its own truncation markers and SHALL cite the raw path.

#### Scenario: Excerpt cites its source

- **WHEN** a test failure appends log context
- **THEN** the appended text names the raw operation path it was derived from

### Requirement: CI artifact handoff is an explicit external dependency

On hosted runners the state directory dies with the job, so attributing a CI failure after the fact additionally requires uploading the operation-log tree as a runner artifact. That upload is a workflow change outside this product change. The product change SHALL write complete local logs without assuming any upload; the durable-attribution claim for hosted runs holds only once the workflow dependency lands.

#### Scenario: Missing upload bounds the claim

- **WHEN** the workflow upload is absent
- **THEN** post-mortem correlation is available on the executing seat only, and no product assertion depends on the uploaded copy
