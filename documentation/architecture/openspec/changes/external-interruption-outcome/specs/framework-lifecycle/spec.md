## ADDED Requirements

### Requirement: Conduct finalizes unobservable outcomes after teardown proof

`conduct` SHALL map a backend `Unobservable` report through teardown
and report the finalized typed outcome as a driver-level report
(typed one-line stderr rendering, exit 1 — the existing class for
paths that cannot report a harness disposition), never as a harness
disposition (never exit 128+N, never exit 0). The rendering SHALL
name the observation (never a proven cause), state that the harness
outcome is unobservable, render the raw client result as
`exited(code)` or `signaled(n)` without coercion, and print
`cleanup=converged` only after actual final proof — otherwise the
dominant residue, observation-error, or shutdown-uncertain outcome
reports instead. Final absence facts (unit file, explicit service
state with checked query, container-exists exit 1) SHALL hold
exactly as in the converged path. No product stderr tail is
rendered (none exists); test-side tap tails appear in test failure
output only.

#### Scenario: Unobservable conduct reports typed, exits 1
- **WHEN** conduct's await observes container-absent-at-query and
  teardown converges with absence proven
- **THEN** conduct prints the typed observation report and exits 1;
  absence facts hold; no harness signal disposition and no proven
  cause are claimed

#### Scenario: Residue dominates the observation report
- **WHEN** teardown after an observed interruption leaves residue
  or ends uncertain
- **THEN** the residue/uncertain outcome reports instead; no
  `cleanup converged` prints

#### Scenario: Boundary asserts observation set per interleave
- **WHEN** the live CLI-vs-attached boundary observes an attributed
  interleave
- **THEN** it asserts the observation record plus explicit absence
  facts plus retained raw (test taps), pinned per interleave;
  exact raw dispositions remain pinned at unit level
