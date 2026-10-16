## Why

The live CLI-vs-attached convergence boundary (QA 0/10 at `a00c882`)
proved conduct cannot honestly report a harness disposition when its
container is externally removed mid-await: the foreground exec client
reports 255/container-gone (harness outcome unobserved), and the
generated stop path (`ExecStop: podman rm -f`) is class provenance
evidence for observed client exit 137 (harness raw wait unavailable —
never a per-run signal claim). The fixed-143 premise is withdrawn.
Conduct needs a typed way to report outcome-unobservable execution
that retains the raw client result — never a fabricated harness
signal disposition and never a bare allow-list of raw statuses.

## What Changes

- New typed execution outcome for outcome-unobservable await,
  split in two phases: backend `Unobservable` (observation record +
  raw client result, no cleanup claim — await precedes teardown)
  and framework-finalized report after teardown + absence proof.
  The harness outcome is explicitly unobservable; the report
  claims co-occurrence facts only, never proven external cause,
  and the converged-cleanup proof (or the dominant
  residue/uncertain outcome) closes the report.
- `conduct` reports the typed outcome (with a documented CLI exit
  mapping) instead of passing through raw client statuses or
  guessing a harness disposition.
- Live boundary asserts the observation set (record + cleanup
  proof + retained raw) per interleave; exact raw
  dispositions stay pinned at unit level per interleave shape.
- No change to the unlink-NotFound repair (independent, already
  shipped); no bare `{137,143,255}` acceptance anywhere.

## Capabilities

### New Capabilities
- (none — outcome taxonomy lives in existing capabilities)

### Modified Capabilities
- `isolator-contract`: await-result outcome taxonomy gains the observational unobservable-harness outcome (record + retained raw status, never causal).
- `framework-lifecycle`: conduct disposition policy for outcome-unobservable executions (typed report + CLI exit mapping; never fabricated harness disposition).

## Impact

- `ExecutionOutcome` taxonomy (`src/framework/isolator.rs`), wire
  `await_result` envelope, conduct disposition/exit mapping
  (`src/main.rs`), live boundary test asserts.
- Specs: `isolator-contract`, `framework-lifecycle` deltas.
- Reviews: Tier-1 → Tier-2 (Advisor shaped the contract question).
