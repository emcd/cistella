# Design: add-operation-logging

## Context

Cistella lifecycle paths (`conduct` install/start, `terminate`, `teardown`, `gc`) mutate shared host state (unit files, containers, attestations) under one HOME, one systemd manager, one container storage, and one creation lock. When a session breaks (e.g. the instrumented honest-red: container gone after `podman restart`, unit file gone by post-mortem), no durable record names the actor — concurrent global `gc` sweeps overlapped the window, but timing is not a mutation receipt. The surgery diagnostics collector captures per-poll evidence but performs no logging itself and must stay read-only.

Stakeholders: operator (merge gate), Advisor (correctness review), tier-1 (change review). Prior guidance (recorded): durable root under XDG_STATE_HOME, secure-path discipline reused, injected test metadata validated and never origin proof, logger failures must not influence decisions.

## Goals / Non-Goals

Goals:

- Every mutating lifecycle operation appends a durable, structured, bounded record: what ran, what it observed, what it decided, what resulted.
- The gc actor gets per-target records (plan, classification, action, result), not just a reaped list.
- A later failure (e.g. the surgery collector's post-mortem) can correlate against operation logs instead of inferring actors from timing.

Non-Goals:

- No change to gc scan/reap behavior, ownership, eligibility, or fail-closed semantics. No change to any product decision path.
- No log-based alerting, remote shipping, or query service. Logs are files; correlation is done by readers (tests, post-mortems, humans).
- No expansion of the read-only diagnostics collector into a logging subsystem; the collector stays a reader.

## Decisions

### 1. Durable root under XDG_STATE_HOME; runtime stages at most bounded coordination

`XDG_STATE_HOME/cistella/logs/<operation-id>/` (fallback `~/.local/state/cistella/logs/`), one directory per operation, length-prefixed frame records inside. Rationale: XDG_RUNTIME_DIR is ephemeral and cannot be the sole home of investigation evidence that must outlive the operation; Advisor and tier-1 converge on this. The runtime directory may hold bounded staging files (e.g. an open file handle target) but never the only copy; any runtime-only retention names its weaker lifetime and requires export before teardown.

Alternative considered: single shared log file. Rejected: concurrent writers from parallel tests would interleave records; per-operation directories give natural isolation and bounded cleanup units.

### 2. Reuse the secure-path discipline, not an existing tree

Creation reuses the repo's anchored-creation helpers (0700 directories, 0600 files, no symlink follow, uid-keyed ownership). New logs do NOT go inside `cistella-state-<uid>` and do not mirror the lease layout: purpose-built tree, same discipline.

### 3. Operation identity is generated; test metadata is injected and untrusted

Each operation mints its own id from a path-safe alphabet (lowercase alphanumerics, fixed length, sufficient entropy) and records a parent id where nested. Test-run id, exact SHA, test name, and parent operation id arrive only by explicit injection, are validated and length-bounded, and are recorded as claims, never as proof of origin. The envelope's argv label comes from a fixed operation vocabulary, never a rendered command line, so no untrusted path text lands in a label. Build provenance is limited to what exists at compile time today (`CARGO_PKG_VERSION`; no baked SHA, no build.rs) and is labeled as such. Labels and paths derived from validated ids only.

### 4. Record shape: envelope, per-target decisions, and only what was observed

Envelope per operation: operation id, parent id, actor (verb + role), pid + process start identity, argv label, start/end wall and monotonic clocks, outcome. Per-target entries for gc: session id, attempt/reconciliation key, unit name + InvocationID, full container id, observed state (exists code, inspect disposition, unit props), decision with reason code, action and result/errno. The gc phases map directly onto source structure: phase-1 classification (`to_reap` / `live_sessions`), phase-2 teardown + `podman rm -f` per classified orphan, tmpdir and attestation phases after. Other operations (install/start, terminate, teardown) use the same envelope with operation-specific observed/decided fields. Start record opens the log; the terminal record closes it; body records carry sequence numbers.

A record carries exactly the observations the deciding code path already made, with explicit `not-queried`/`unavailable` markers for fields the path did not read. No additional backend queries are issued to fill log fields and the decision is never re-evaluated after the fact. Rationale: gc reads fields conditionally, so InvocationID, container id, and exists code are not always available; inventing reads to complete the log would change the backend load and timing of the observed path. Any extra observation beyond the decision's own reads would be a separately bounded optional probe — this design includes none.

### 5. Logger failures are observability losses with a bounded, disclosed perturbation

A write failure, partial record, or concurrent-writer interleave is recorded (or noted as missing) and MUST NOT flip a reap decision, a safety outcome, failure propagation, or fail-closed behavior. Logging sits beside the decision, after it; gc's inspect-failure abort path is untouched by logging code. New input/output, pruning, and fsync change timing, blocking, and lock scope, so the promise is unchanged decision semantics with bounded disclosed perturbation — one append per decision point, fsync on close only, prune best-effort off the hot path — never zero runtime effect and never "negligible" as fact. Storage is length-prefixed frames (4-byte big-endian length, maximum frame payload 256 KiB, checks against the maximum and the remaining file before allocation) written by the single owning writer of each operation file (no multi-writer joins; concurrent operations use separate files), so a torn tail is definitively detectable and handled by the stated reader contract: discard the torn frame and every byte after it, preserve the valid prefix, record the discard in the derived result, and read a missing terminal as UNKNOWN. On detecting a partial write the writer stops that stream and marks the loss (best-effort UNKNOWN terminal where writable) rather than appending behind the torn frame. The deterministic logger-fault pin exercises the actual gc decision/action path with injection at the real writer seam, not permission heuristics or a duplicated logger-only model.

### 6. Bounded total retention with admission control and a reserved failure budget

The hard logical budget is 3 GiB of operation bytes for the whole tree. Admission control enforces it: opening an operation reserves its metadata share and each frame reserves its length before writing, covering operation bytes, failure and incomplete operations, active operations, and eviction-accounting records, with bounded coordination. Empty and abandoned directories count as metadata against the budget. The physical filesystem may transiently overshoot the logical budget by at most one in-flight frame per active writer — that per-writer frame allowance is the only permitted excess, and it bounds the overshoot arithmetically. When the budget cannot be secured, further logging for that operation stops (or the open is rejected) while the business operation proceeds unchanged; the loss is observability only.

Inside the tree, a reserved failure budget of 1 GiB is protected: success-outcome operations are pruned by count (most recent 2000) and by age (7 days, whichever is smaller) without consuming the reserve; failure-outcome and incomplete operations are exempt from count pruning, bounded by the 1 GiB reserve and the 7-day age bound. Exhaustion evicts oldest-first with one accounting record per eviction run. Incomplete operations (no terminal record) count against the failure reserve and age out by 7 days; active ownership is pid plus process start identity (no mtime or PID-reuse proof), and inactive-incomplete operations are eligible victims so abandoned empty directories are never immortal. The pruner skips actively-written operations, bounds its scan work per run, acquires no lifecycle locks, and never waits unboundedly. No secrets, no full ambient environment in records. Post-close eviction is maintenance, not enforcement — the admission protocol is what makes the cap real.

### 7. Raw logs separate from excerpts

The log file is a sequence of length-prefixed frames and is never rewritten. Any human or test excerpt (e.g. attached to a failure report) is a derived rendering with its own truncation markers; the raw path is always cited alongside. The reader contract (discard-and-record torn tails, UNKNOWN for missing terminal records) is normative in the spec; framing only makes the torn case definitively detectable.

## Risks / Trade-offs

- [Observer perturbation] Writing during gc perturbs the timed path it observes → Mitigation: single append per decision point from the decision's own reads (no extra backend queries), fsync on close only, prune best-effort off the hot path; the perturbation is bounded and disclosed, never zero by claim.
- [Disk exhaustion] Logs on a busy seat, including failure accumulation → Mitigation: total-tree 3 GiB budget with a protected 1 GiB failure reserve, per-op 1 MiB cap, eviction with accounting records, best-effort prune-on-close plus age bound.
- [Concurrent writers] Parallel tests append under one HOME → Mitigation: per-operation directories with a single owning writer each (no shared file, no multi-writer joins), anchored creation, prune skips active operations.
- [Attribution overreach] A log entry is evidence, not proof (missing logs, crash gaps, bounded lifetime, unenumerated actors) → Mitigation: requirements state correlation limits; incomplete operations read UNKNOWN; no claim stronger than the record.

## Migration Plan

Additive only: new directories and files, no migration, no rollback beyond deleting the tree. Readers (tests, post-mortems) adopt opportunistically; absence of logs never fails a pre-existing assertion. The motivating CI use case additionally depends on a workflow change outside this proposal: uploading the operation-log tree as a runner artifact (alongside the existing suite/audit logs). Until that workflow change lands, durable attribution holds for the local seat only.

## Open Questions

- Whether the matched `systemctl restart` diagnostic (separately commissioned) should be the first consumer that correlates against these logs — suggested yes, but owned by that change, not this one.

Scope note: this change covers gc plus conduct install/start, terminate, and shared teardown in one contract by explicit operator expansion (operator decision 2026-10-26: generalized logging support with gc logging as its first consumer, full lifecycle logged so the failure has enough data). It is reviewed as the larger design, not the small-gc vehicle.
