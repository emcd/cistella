## 1. Observation record + corroboration (no stream changes)

- [x] 1.1 Add `Unobservable{observation, raw}` (backend, no cleanup
  claim) + finalized (framework, post-proof) shapes to the taxonomy
  shared by reference and wire backends; wire variant gated on
  hello capability `await-interrupted-v1` (emit only when negotiated
  both ways, legacy mapping otherwise, unsolicited emission
  refuses).
- [x] 1.2 Attribution by read-only corroboration (implemented in `corroborated_outcome`; `container_exists_code` strict) at the await
  boundary (container-exists strict codes, service state, unit
  presence, query ordering recorded); never string-match client
  stderr; container-present failures keep ordinary mapping.
- [x] 1.3 Negative-control pins (`tests/unit/unobservable.rs`, deterministic): ordinary Exited 137/143/255,
  spoofed-text-255-present, signaled-with-container-present keeps
  legacy Signaled mapping (explicit compatibility policy, receipt
  unproven), normal-nonzero-then-post-completion-removal
  (observational, no causal upgrade),
  unrelated-signal-during-concurrent-stop.
- [x] 1.4 EXACT hello outcomes pinned (`offered_capability` + encode gate + unsolicited-refusal decode; reference native): negotiated-both-ways → typed;
  new-host/old-guest, unnegotiated-new/new, old/old → legacy both
  sides; old-host/new-guest safe by guest MUST-NOT-emit;
  NEGATIVE unsolicited-emission-without-negotiation → host
  refuses. Reference backend (no hello) implements typed natively;
  conformance pairs it with negotiated wire guests + separate
  legacy-parity pins. Fields/bounds, unknown-tag refusal,
  replay-until-remove.

## 2. Conduct mapping + boundary + rendering

- [x] 2.1 Conduct finalizes post-teardown (`main.rs` Unobservable arm; `format_unobservable_report` in `runtime.rs`): typed one-line stderr
  report + exit 1; residue/uncertain dominate; `cleanup=converged`
  only after proof; no product tail rendered.
- [x] 2.2 Golden renderer pins (incl. hostile-string escaping): exited/code vs signaled/n forms,
  observation naming, no causal language, no terminal injection.
- [x] 2.3 Reshape the live boundary to the observation set per
  interleave (exit 1 + typed report + cleanup=converged + absence
  facts; fixed-143 premise withdrawn; compiles + gates, live is
  2.4).
- [ ] 2.4 QA live loop incl. boundary on both backends; code-exact
  container checks + explicit service strings.

## 3. Review and sync

- [ ] 3.1 Tier-1 review → Tier-2 review.
- [ ] 3.2 Sync both spec deltas to main specs + changelog.
