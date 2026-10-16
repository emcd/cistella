## 1. Observation record + corroboration (no stream changes)

- [x] 1.1 Add `Unobservable{observation, raw}` (backend, no cleanup
  claim) + finalized (framework, post-proof) shapes to the taxonomy
  shared by reference and wire backends; wire variant gated on
  hello capability `await-interrupted-v1` (emit only when negotiated
  both ways, legacy mapping otherwise, unsolicited emission
  refuses; guest advertises ONLY when offered — old-host admission
  negotiates cleanly).
- [x] 1.2 Attribution by read-only corroboration (implemented in `corroborated_outcome`; `container_exists_code` strict) at the await
  boundary (container-exists strict codes, service state, unit
  presence, query ordering recorded); never string-match client
  stderr; service-active-and-present + unobtainable keep ordinary
  mapping. Decision lives in `isolators::corroboration`
  (backend-domain), taxonomy only in framework.
- [x] 1.3 Negative-control pins (`tests/unit/unobservable.rs`, deterministic): ordinary Exited 137/143/255
  present-and-active, spoofed-text-255-present-and-active,
  signaled-present-and-active (compatibility policy),
  deactivating-service-present-container (observational, Mode B),
  empty-service (legacy), normal-nonzero-then-post-completion-removal
  (observational, no causal upgrade),
  unrelated-signal-during-concurrent-stop. Plus `AbsenceProof::converged`
  rule pins and renderer present-container golden.
- [x] 1.4 EXACT hello outcomes pinned (real guest binary: offered→advertises,
  unoffered→role-only, old admission negotiates; `offered_capability` +
  encode gate + unsolicited-refusal decode incl. mixed-tag/extra-field/
  success-zero controls; reference native): negotiated-both-ways → typed;
  new-host/old-guest, unnegotiated-new/new, old/old → legacy both
  sides; old-host/new-guest safe by guest MUST-NOT-emit;
  NEGATIVE unsolicited-emission-without-negotiation → host
  refuses. Fields/bounds, unknown-tag refusal,
  replay-until-remove.
- [x] 1.5 Completion freeze: claim-once + bounded replay-wait
  (`podman_await` module); barrier-synchronized two-awaiter race
  + cache-replay pin (inline: tables crate-private by design).

## 2. Conduct mapping + boundary + rendering

- [x] 2.1 Conduct finalizes post-teardown (`finalize_unobservable` in
  `framework::conduct`): ANY teardown error dominates, then release
  failure, then backend `prove_absence` re-query; `cleanup=converged`
  only on the converged bit; typed one-line stderr report + exit 1;
  preexisting transition-failure precedence preserved in `main.rs`.
- [x] 2.2 Golden renderer pins (incl. hostile-string escaping + present-container truthfulness): exited/code vs signaled/n forms,
  observation naming, no causal language, no terminal injection.
- [x] 2.3 Reshape the live boundary to the observation set per
  interleave (exit 1 + typed report + cleanup=converged + absence
  facts; fixed-143 premise withdrawn; compiles + gates, live is
  2.4).
- [ ] 2.4 QA live loop incl. boundary on both backends; code-exact
  container checks + explicit service strings.

## 3. Review and sync

- [x] 3.1 Tier-1 review → Tier-2 review. (Packet Tier-1 APPROVE ×3 + Tier-2 observational approval; implementation Tier-1 APPROVE; committed `ed1c800`, local, unpushed.)
- [ ] 3.2 Sync both spec deltas to main specs + changelog.
