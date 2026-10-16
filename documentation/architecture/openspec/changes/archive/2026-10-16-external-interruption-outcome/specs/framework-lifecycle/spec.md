## ADDED Requirements

### Requirement: Conduct finalizes unobservable outcomes after teardown proof

`conduct` SHALL map a backend `Unobservable` report through teardown
and report the finalized typed outcome as a driver-level report
(typed one-line stderr rendering, exit 1 — the existing class for
paths that cannot report a harness disposition), never as a harness
disposition (never exit 128+N, never exit 0). Proof dominance SHALL
hold in phase order — teardown, then guest release (quiescence),
then fresh backend proof, then render: any teardown error
dominates, then release failure, then backend absence re-proof —
observation errors and unconverged proofs dominate with their own
reports, and `cleanup=converged` prints only on the backend's
converged bit. The absence proof SHALL re-query container, service,
and unit with checked queries (manager failure, unexpected
shapes, and non-NotFound metadata errors are observation errors);
convergence requires container-exists exit 1 AND an explicit
terminal service state (`LoadState` `not-found` or `ActiveState`
`inactive`) AND absent unit file — every other service shape
(active, activating, deactivating, failed, empty) fails
convergence, and a stopped-but-present container never converges.
Proof attests native backends only: registered (foreign) guests
refuse explicitly, since local queries prove nothing about their
resources. A preexisting hooked exec/transition failure SHALL keep
its disposition precedence over the unobservable arm. The
rendering SHALL name the observation (container fact from the
recorded code, never fixed), state that the harness outcome is
unobservable, render the raw client result as `exited(code)` or
`signaled(n)` without coercion, and carry no product stderr tail
(none exists). Final absence facts (unit file, explicit service
state with checked query, container-exists exit 1) SHALL hold
exactly as in the converged path. Test-side tap tails appear in
test failure output only.

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
