## ADDED Requirements

### Requirement: Single roster source of truth
The quarantined live-test set SHALL be enumerated in exactly one versioned roster file listing fully-qualified test paths, one per line. No other file may define membership.

#### Scenario: Roster is the sole membership authority
- **WHEN** a reviewer asks which tests are quarantined at a given tip
- **THEN** the answer is exactly the roster file's contents at that tip, and CI selection is derived from it

### Requirement: Disjoint gating and informational sets
The gating live selection and the informational flaky selection SHALL be disjoint: no live-gated test runs in both tiers at the same tip.

#### Scenario: No test runs gated and informational simultaneously
- **WHEN** the roster is resolved into the two nextest `-E` selection expressions at a tip
- **THEN** their intersection is empty, verified by execution output, not by reading config

### Requirement: Union covers the live universe
The union of the gating and informational selections SHALL equal the full live-gated universe (currently 90 tests: 89 in `tests/integration/`, 1 in `tests/unit/lock_teardown.rs`).

#### Scenario: Roster accounts for every live-gated test
- **WHEN** `cargo nextest list` enumerates the live profile at the tip
- **THEN** every listed live test is selected by exactly one of the two tiers, and the counts reconcile (no silent omissions)

### Requirement: Execution proof at the exact tip
Roster accounting SHALL be proved by execution, not by reading configuration: the CI record must show the selected sets actually running (or explicitly skipped with reason) at the exact tip under review.

#### Scenario: Selection is demonstrated, not asserted
- **WHEN** a review packet claims a given roster
- **THEN** it cites the run in which `nextest list` (or the test run itself) shows every quarantined test selected/executed or explicitly skipped with reason, plus a preserved full-live audit route

### Requirement: Informational failures stay visible
The flaky tier SHALL run as an informational, non-gating leg whose failures are retained, surfaced, and tracked. Failures MUST NOT be swallowed as PASS and MUST NOT be retried to green.

#### Scenario: Flaky red is evidence, not noise
- **WHEN** a quarantined test fails in the informational leg
- **THEN** the failure is recorded against its issue with the run ID and raw output, the gating legs stay green-or-red on their own merit, and no rerun is requested to clear it

### Requirement: Gating legs stay required
Ordinary required jobs SHALL remain required. Quarantine MUST NOT turn the workflow informational as a whole.

#### Scenario: Gate keeps its teeth
- **WHEN** the quarantine change lands
- **THEN** the gating live legs still fail the run on any failure, and only the explicitly quarantined tests are absent from them

### Requirement: Per-test move process
Moving a test into or out of quarantine SHALL require a per-test decision row (property, current failure plus raw evidence, staging-vs-product attribution with named evidence basis, current gating coverage, proposed coverage delta, owner, issue, reentry condition) and explicit reviewer approval. Aggregate lists and "move on next flake" SHALL NOT constitute preapproval.

#### Scenario: No silent or bundled moves
- **WHEN** a packet proposes quarantine changes
- **THEN** each test carries its own row and its own approval; an empty initial set is valid and moves nothing

### Requirement: Reentry condition
A quarantined test SHALL return to the gating set only when its fix lands with evidence: the fix commit plus green execution on the affected scope, and confirmation in a subsequent full-matrix run.

#### Scenario: Return is earned, not timed out
- **WHEN** a quarantined test's root cause is fixed
- **THEN** reentry cites the fix and the green runs; expiry alone never returns a test

### Requirement: Filter discipline for diagnostics
Targeted diagnostic runs SHALL use the intended profile/filter combination or `--ignore-default-filter`: a bare `-E` expression MUST NOT silently omit quarantined tests from the intended universe.

#### Scenario: Diagnostics cover what they claim
- **WHEN** a diagnostic run filters the live suite
- **THEN** its packet states the effective selected set, and any omission of quarantined paths is explicit
