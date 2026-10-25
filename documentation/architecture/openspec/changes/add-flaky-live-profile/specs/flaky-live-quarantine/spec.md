## ADDED Requirements

### Requirement: Single roster source of truth
The quarantined live-test set SHALL be enumerated in exactly one versioned roster file listing full test names as shown by `cargo nextest list` (module path without binary prefix, e.g. `transport::transport_harness`), one per line. No other file may define membership.

#### Scenario: Roster is the sole membership authority
- **WHEN** a reviewer asks which tests are quarantined at a given tip
- **THEN** the answer is exactly the roster file's contents at that tip, and CI selection is derived from it

### Requirement: Disjoint gating and informational sets
The gating live selection and the informational flaky selection SHALL be disjoint: no live-gated test runs in both tiers at the same tip. Both selections are computed over the live-gated subset only: a test counts as selected if and only if its `filter-match.status` is `matches` AND its `ignored` flag is true, under the same profile and `--run-ignored=all` invocation as the legs (a roster check that omits the profile or the flag enumerates no live-gated tests and proves nothing).

#### Scenario: No test runs gated and informational simultaneously
- **WHEN** the roster is resolved into the two nextest `-E` selection expressions at a tip
- **THEN** their intersection over the live-gated subset is empty, verified by execution output, not by reading config

### Requirement: Informational selection equals the roster
The informational selection SHALL equal the roster exactly, in membership and in count. A roster entry that matches nothing (typo, stale path, renamed test) MUST fail the roster check even when the union check would otherwise pass — otherwise a stale entry silently quarantines nothing while claiming coverage is preserved.

#### Scenario: Stale roster entry fails closed
- **WHEN** the roster contains a path no live-gated test has at the tip
- **THEN** the informational selection is missing that member, the equality check fails, and the workflow fails loudly instead of running an empty quarantine

### Requirement: Independent authoritative baseline
Roster accounting SHALL compare each tier's actual-profile selection against an INDEPENDENT baseline that bypasses tier, roster, and default filters: `cargo nextest list` with `--ignore-default-filter`, identifying the intended live-gated subset by composite identity (binary identity plus full test name, so duplicate names cannot collapse). A profile default-filter that excludes a test MUST NOT be able to shrink the baseline, the selections, and the union check together. The baseline also preserves the ordinary non-ignored gating tests: the live job executes those, not only the ignored 90, so the union check covers the full pre-change gating selection. With an empty roster, the proof compares the full pre-change gating selection, not just the ignored subset.

#### Scenario: Default-filter omission fails the check
- **WHEN** a profile default-filter would omit a live-gated test from discovery
- **THEN** the baseline (which ignores the default filter) still contains it, the union against the baseline fails, and the workflow fails loudly instead of silently dropping the test

#### Scenario: Empty roster proves bit-identity
- **WHEN** the roster is empty
- **THEN** the gating selection equals the full pre-change gating selection (ignored and non-ignored alike), proving the mechanics change nothing about what runs gated

### Requirement: Roster format is exactly selectable
Roster entries SHALL be full test names as shown by `cargo nextest list` (module path without binary prefix, e.g. `transport::transport_harness`). Plain `test(<full-name>)` selection MUST match exactly one test: full names are unique across the workspace and no live-gated name is a substring of any other test name (verified at this tip; re-verified by the roster check, since a collision or rename surfaces as a count or equality failure). Leaf-only names MUST NOT be used: leaf names collide across binaries (e.g. `hello_version_mismatch_refuses` exists in two suites) and partial module qualification does not match.

#### Scenario: Roster entries select exactly one test each
- **WHEN** the roster is resolved into per-entry `test(<name>)` predicates joined by `or`
- **THEN** each predicate matches exactly one live-gated test, and the informational selection count equals the roster line count

#### Scenario: Stale or colliding entry fails closed
- **WHEN** a roster entry matches zero tests (stale path) or more than one test (collision after a rename)
- **THEN** the membership/count equality check fails and the workflow fails loudly

### Requirement: Predicates validated against the raw discovered set
Each roster predicate SHALL be validated against the RAW discovered test set — every test in every binary, before projecting to ignored/live. The predicate MUST match exactly one composite test identity (binary identity plus full test name), that identity MUST be live-eligible (ignored live-gated), and duplicates, duplicate roster lines, zero matches, and overmatches SHALL be rejected before any selection is used. Live-subset equality alone does not prove this: an entry matching one ignored live test plus one ordinary test would pass every live-subset check while the gating exclusion silently removed the ordinary test.

#### Scenario: Ordinary-test substring collision fails closed
- **WHEN** a roster entry's predicate matches an ordinary (non-live-gated) test in addition to, or instead of, its intended live-gated test
- **THEN** the per-predicate validation rejects the roster before selection, naming the colliding identities, and the workflow fails loudly

### Requirement: Execution proof at the exact tip
Roster accounting SHALL be proved by execution, not by reading configuration: the CI record must show the selected sets actually running (or explicitly skipped with reason) at the exact tip under review, computed over the live-gated subset under the same profile and `--run-ignored=all` as the legs.

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

### Requirement: Quarantined tests keep full platform coverage
Every quarantined test SHALL still execute on each OS/arch leg it formerly covered (all four matrix legs). The flaky leg runs the full matrix; the roster check and the union proof are REQUIRED gates outside any informational continue-on-error boundary — only the test failures inside explicitly informational executions may be non-gating. A per-test decision may explicitly accept a platform coverage reduction, with the same explicit approval as any other coverage delta.

#### Scenario: Platform coverage is preserved by default
- **WHEN** a test is quarantined
- **THEN** it executes on all four legs in the informational tier, and any leg it skips names the explicit per-test approval accepting that reduction

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
