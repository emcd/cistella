## 1. Ancestor computation (pure)

- [x] 1.1 Preparation candidates helper: per-target walk stopping at tmpfs home or `/`, de-duplicated, lexical mountpoint and beneath-bind exclusion (nested/overlapping chains pinned), with unit tests. Lives in `src/prepare.rs` (pre-commit linecheck split).
- [x] 1.2 Containment authorization predicate plus `volume_targets` (canonical emitted bind set incl. credential surface) and `parse_stat_map`, with unit tests: symlinked-into-mount refused, nested-RO descendant excluded, tmpfs-home descendant allowed, noncanonical spellings canonicalized, spaced names parsed.

## 2. Preparation step (live)

- [x] 2.1 Wire post-start, pre-harness preparation into `conduct_session`: canonical bind set, live seat-uid read, batched bounded execs (uid/resolve/mkdir/stat/conditional-chown), guard-live `teardown_inner` + residue decision on failure.
- [x] 2.2 Live cases: multi-level deep-target sibling creation (spike reproduction), host-aliased source conducts normally, RO-subtree-untouched alongside tmpfs-chain preparation, noncanonical spelling prepared, forced-timeout (1 ms) fails closed with zero residue. The container-side adversarial alias has no live form — nesting under any triple is an overlap violation, so the runtime refusal path covers only image-baked symlinks and is proven by unit tests on the authorization predicate (including resolved-binds-win-over-lexical). `/tmp` and `/run` pinned untouched live; `preparation_sources` split unit-tested.

## 3. Validation and review

- [x] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through tier-1 review (Reviewer General) plus a QA Partner test-focus heads-up (deep-target sibling creation) — tier-2 only if tier-1 flags boundary concerns — then sync, archive, and push on approval.
