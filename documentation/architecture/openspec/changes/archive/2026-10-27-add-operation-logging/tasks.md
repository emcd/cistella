## 1. Log writer

- [x] 1.1 Add a small operation-log writer: anchored dir/file creation (0700/0600, no symlink follow) under XDG_STATE_HOME/cistella/logs/<op-id>/ with ~/.local/state fallback, minted path-safe op ids, single owning writer per file, length-prefixed frames (4-byte length, 256 KiB max payload, pre-allocation checks) with sequence numbers, start plus terminal records
- [x] 1.2 Add admission control plus best-effort prune-on-close under the total budget (3 GiB tree, 1 GiB protected failure reserve, per-op 1 MiB cap, owner-attributed reservation files plus nonblocking admission lock, release on close and on drop, dead-owner stale reaping, stop-or-reject logging when unsecurable while the business op proceeds, 2000 success ops or 7 days, failure/incomplete exempt from count pruning, eviction with accounting records, inactive-incomplete eligible via pid plus process-start-identity liveness, active operations skipped, finalizing marker held across terminal write plus fsync with marker-bearing live-owner skip, completed-without-marker policy-eligible including own maintenance pass, bounded scan, no lifecycle locks, silent failure)
- [x] 1.3 Add injected-metadata intake (`CISTELLA_TEST_RUN_ID`, `CISTELLA_SHA`, `CISTELLA_TEST_NAME`, `CISTELLA_PARENT_OP_ID`, 128-char bound with truncation marker, unknown verbs mapped to the fixed `unknown` label) recorded as claims; argv labels from the fixed operation vocabulary

## 2. GC integration

- [x] 2.1 Emit phase-1 classification records (per-candidate observed state plus to_reap/live sets with reason codes, not-queried markers for unread fields, no extra backend queries)
- [x] 2.2 Emit phase-2 per-target action records (stop, unlink + reload result, container removal, attestation outcome, errno on failure)
- [x] 2.3 Emit tmpdir and attestation phase records; place all logging beside decisions so logger failure cannot change outcomes or failure propagation

## 3. Lifecycle integration

- [x] 3.1 Emit install/start records from conduct paths (unit installed vs adopted, start outcome)
- [x] 3.2 Emit terminate/teardown records (stop, unlink + reload, attestation reaped or retained)

## 4. Reader

- [x] 4.1 Add a log reader implementing the stated contract (declared schema and state machine: start-only-first, bodies-while-open, at most one terminal last; max-payload and remaining-file checks before allocation; byte-bounded reads that refuse oversize files; partial-prefix/short-payload/invalid-JSON/invalid-schema/sequence-gap/missing-start/missing-terminal handling with valid-prefix preservation; torn-tail discard with discard record; missing terminal reads UNKNOWN, incomplete ops labeled UNKNOWN)
- [x] 4.2 Add writer partial-write handling (stop the stream, best-effort UNKNOWN terminal on an intact stream or separate loss evidence, never append behind a torn frame) plus the finalizing marker held across terminal write plus fsync and renamed to finalized evidence before maintenance

## 5. Verification

- [x] 5.1 Backend-free pins for writer and reader semantics (framing, sequence, max payload, truncation markers, torn-tail discard accounting, five discriminating reader fixtures for oversize/short-payload/invalid-JSON/missing-start/missing-terminal, incomplete-operation UNKNOWN meaning, prune victim selection and eviction accounting, anchored prune safety with symlink and ownership checks, admission reservation plus stop-or-reject-logging plus release-on-close plus stale reaping, concurrent admission bounds, live-incomplete skip plus dead-incomplete eviction plus marker-protected finalization plus valid self-eviction-with-marker fixtures on the actual policy), UTF-8 boundary truncation plus injected-claim validation pins, scoped-error-is-failure plus alongside-success-victim pin, select/evict unlink-time recheck pin with planted marker and counted skips, contention defers-victim pin, dead-owner via reaped child plus aged-empty-directory plus pre-construction-release pins, nofollow plus regular-file eviction-marker and admission-lock pins, classify abort receipts plus marker fields plus parent linkage plus role plus end-timestamp pins, finalized-evidence wiring pin, declared state-machine rejection plus bounded reads, unknown-verb fixed label, explicit-id validation, observed-state mapping plus missing-vs-never-queried pins, classify-candidate producer pin driven with missing and empty property maps, contended-close-omits-terminal pin, close-triggers-maintenance-with-aged-victim pin, select-then-evict interleave pin, FIFO and symlink admission/eviction pins, reaped-child-liveness and aged-empty-dir pins, pre-construction reservation-release pin, non-blocking verified-regular control reads plus FIFO reservation-skip plus invalid-reservation-charged plus bounded-listing-refusal plus FIFO-records prune pins, ps spawn-failure receipt pin, teardown sibling-filter backend-free control, fresh-seat missing-chain creation plus symlink-ancestor refusal pins, fd-anchored component walk plus intermediate-link missing/existing-leaf pins)
- [x] 5.2 Live pins: gc reaps a fixture orphan and the log reconstructs classification plus per-step results; deterministic logger-fault injection at the real writer seam leaves gc decisions and failure propagation identical
- [x] 5.3 `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `openspec validate --all --strict` clean
