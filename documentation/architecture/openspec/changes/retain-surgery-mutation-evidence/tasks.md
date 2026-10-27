## 1. Retained mutation identity

- [ ] 1.1 Add `mutation_report` to `LandingEvidence`, built at the `run_surgery_cycle` success site from argv, successful exit status, and both streams through `escape_capped` (cap 4000 kept characters per stream, following `POSTMORTEM_STREAM_CAP`)
- [ ] 1.2 Render the mutation section first in the guard report at the `enter_gate.rs` call sites (mutation, then history, then baseline), verdict semantics unchanged

## 2. Backend-free pins

- [ ] 2.1 Pin the seam with the injected runner/clock: a later failure after a successful mutation renders the mutation's argv, status, and capped streams
- [ ] 2.2 Pin truncation marking with an over-cap stream and the no-mutation-change case (failed mutation still returns no evidence)

## 3. Validation

- [ ] 3.1 Full gates green (unit, fast suite, clippy, fmt, openspec validate) with no existing pin broken
