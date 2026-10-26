## 1. Log writer

- [ ] 1.1 Add a small operation-log writer: anchored dir/file creation (0700/0600, no symlink follow) under XDG_STATE_HOME/cistella/logs/<op-id>/ with ~/.local/state fallback, minted path-safe op ids, single owning writer per file, length-prefixed frames (4-byte length, 256 KiB max payload, pre-allocation checks) with sequence numbers, start plus terminal records
- [ ] 1.2 Add admission control plus best-effort prune-on-close under the total budget (3 GiB tree, 1 GiB protected failure reserve, per-op 1 MiB cap, reserve-before-create/append, stop-or-reject logging when unsecurable while the business op proceeds, 2000 success ops or 7 days, failure/incomplete exempt from count pruning, eviction with accounting records, inactive-incomplete eligible via pid plus process-start-identity liveness, active operations skipped, bounded scan, no lifecycle locks, silent failure)
- [ ] 1.3 Add injected-metadata intake (`CISTELLA_TEST_RUN_ID`, `CISTELLA_TEST_NAME`, `CISTELLA_PARENT_OP_ID`, exact SHA; 128-char bound with truncation marker) recorded as claims; argv labels from the fixed operation vocabulary

## 2. GC integration

- [ ] 2.1 Emit phase-1 classification records (per-candidate observed state plus to_reap/live sets with reason codes, not-queried markers for unread fields, no extra backend queries)
- [ ] 2.2 Emit phase-2 per-target action records (stop, unlink + reload result, container removal, attestation outcome, errno on failure)
- [ ] 2.3 Emit tmpdir and attestation phase records; place all logging beside decisions so logger failure cannot change outcomes or failure propagation

## 3. Lifecycle integration

- [ ] 3.1 Emit install/start records from conduct paths (unit installed vs adopted, start outcome)
- [ ] 3.2 Emit terminate/teardown records (stop, unlink + reload, attestation reaped or retained)

## 4. Reader

- [ ] 4.1 Add a log reader implementing the stated contract (max-payload and remaining-file checks before allocation, partial-prefix/short-payload/invalid-JSON/invalid-schema/sequence-gap/missing-start/missing-terminal handling, torn-tail discard with discard record, missing terminal reads UNKNOWN, incomplete ops labeled UNKNOWN)
- [ ] 4.2 Add writer partial-write handling (stop the stream, best-effort UNKNOWN terminal, never append behind a torn frame)

## 5. Verification

- [ ] 5.1 Backend-free pins for writer and reader semantics (framing, sequence, max payload, truncation markers, torn-tail discard accounting, partial-prefix/short-payload/invalid-JSON/schema/sequence inputs, incomplete-operation meaning, prune victim selection and accounting, anchored prune safety, admission reservation and stop-or-reject-logging, concurrent admission plus active-capacity plus prune-failure fixtures on the actual policy)
- [ ] 5.2 Live pins: gc reaps a fixture orphan and the log reconstructs classification plus per-step results; deterministic logger-fault injection at the real writer seam leaves gc decisions and failure propagation identical
- [ ] 5.3 `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `openspec validate --all --strict` clean
