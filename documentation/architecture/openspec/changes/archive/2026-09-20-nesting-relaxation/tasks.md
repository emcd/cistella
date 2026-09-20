## 1. Predicate relaxation (shipped as `7f34fd8`)

- [x] 1.1 Make `overlap_allowed` / `overlap_allowed_host` mode-agnostic (strict nesting stacks; duplicates refuse); update predicate docs and call-site comments.
- [x] 1.2 Rewrite the old-rule unit tests to the new contract; add host-mirror and parent-first ordering tests.
- [x] 1.3 Add live `nested_rw_under_rw_conducts` (wholesale RW ancestor + pair worktree beneath, write round-trips to host).
- [x] 1.4 Update live `specs/mounts/spec.md` inline (requirement body, RW-combination + duplicate-targets scenarios, canonicalize reframed).

## 2. Validation and review

- [x] 2.1 `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, fast suite, full live tier on host (147/147), `openspec validate --all --strict`.
- [x] 2.2 Tier-1 implementation review (approved with parent-first test + two cosmetics folded).
- [ ] 2.3 Retroactive record complete (this change); archive after push.
