## Why

The first honest-red capture (CI 40346550922) settled several hypotheses for one occurrence but left the unit-remover question open: the unit was gone by postmortem with no attributed actor. The surgery collector retains the bounded poll history and the pre-surgery baseline for post-landing failures, but drops the successful mutation's own argv, exit status, and streams once the landing wait passes — so the one fact that names what ran immediately before the death is missing from exactly the reports that need it.

## What Changes

- `LandingEvidence` gains the successful mutation's argv, exit status, and capped stdout/stderr alongside the existing history and baseline reports.
- The post-landing `EvidenceGuard` output includes the retained mutation identity (argv, status, capped streams) with verdict semantics unchanged.
- Backend-free pins drive the real collector seam with an injected runner/clock: a later failure after a successful mutation renders the mutation's identity; truncation markers bound the retained streams.

## Capabilities

### New Capabilities

(none — harness-only evidentiary retention, no new product capability)

### Modified Capabilities

- `surgery-diagnostics`: extend "Landing history retained for later failures" to include the successful mutation's argv, status, and capped streams in the retained set dumped alongside post-landing failures.

## Impact

- `tests/integration/surgery_collect.rs` (`LandingEvidence`, `EvidenceGuard`, `run_surgery_cycle` success path) and its backend-free pins (`surgery_diag/tests.rs` or the collector test module).
- `tests/integration/enter_gate.rs` at both guard arming sites (`:291` and `:446`), which share the identical literal `format!("landing evidence:\n{}\nbaseline:\n{}", ...)` — both grow the mutation section, and the duplication is a drift risk to fix together, not one site at a time.
- No product code, no CI workflow changes, no budget or verdict-semantics changes. Rendered reports grow by a bounded mutation-identity section.
