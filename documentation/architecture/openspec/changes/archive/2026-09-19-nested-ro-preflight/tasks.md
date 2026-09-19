## 1. Preflight check (pure + wiring)

- [x] 1.1 Add `mount.rs` pure helpers: nearest-RO-ancestor selection over canonical targets plus container-suffix translation onto the ancestor host source, with unit tests (depth ordering, symlink-canonical inputs, type expectations).
- [x] 1.2 Wire into `conduct_session` after validation, before scratch creation: existence + `is_dir` per chain component, typed error naming RO ancestor and missing host path, zero mutation on refusal (assert unit/scratch absence in tests).

## 2. Live coverage

- [ ] 2.1 Preexisting nested chain succeeds (child writable, parent RO); missing intermediate refuses pre-mutation with the typed error; session-directory-under-RO dogfood shape passes; symlinked source resolves through; induced race/host change leaves no residue.

## 3. Validation and review

- [x] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through tier-1 review (Reviewer General) plus a QA Partner test-focus heads-up — tier-2 only if tier-1 flags boundary concerns — then sync, archive, and push on approval.
