## 1. Ancestor computation (pure)

- [ ] 1.1 Add a `runtime.rs` helper computing preparation candidates from triple targets + tmpfs home + session-home target: per-target ancestor walk, de-duplicated, lexical mountpoint and `/` exclusion (nested/overlapping chains pinned), with unit tests.
- [ ] 1.2 Add the containment authorization predicate (resolved path beneath no bind-mount target, tmpfs-home subtrees eligible) as a pure function with unit tests: symlinked-into-mount refused, nested-RO descendant excluded, tmpfs-home descendant allowed.

## 2. Preparation step (live)

- [ ] 2.1 Wire post-start, pre-harness preparation into `conduct_session`: live seat-uid read, `podman exec --user root` mkdir/chown per computed ancestor, fail closed via existing teardown on any failure.
- [ ] 2.2 Add live cases: multi-level deep-target sibling creation (the spike reproduction; one level is insufficient), adversarial symlink-to-bind-mount refusal (fail closed, host mount unmutated), and nested-RO exclusion with tmpfs-home eligibility.

## 3. Validation and review

- [ ] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through tier-1 review (Reviewer General) plus a QA Partner test-focus heads-up (deep-target sibling creation) — tier-2 only if tier-1 flags boundary concerns — then sync, archive, and push on approval.
