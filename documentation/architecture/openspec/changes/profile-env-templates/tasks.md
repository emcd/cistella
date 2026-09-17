## 1. Expansion coverage

- [ ] 1.1 Add `env` values and `labels` table values to the scan and substitute passes in `src/profile.rs`, keeping the existing pass order and precedence.
- [ ] 1.2 Reject `{{...}}` spans in label keys with a typed error (keys never expand); confirm env keys need no handling (charset cannot contain braces).
- [ ] 1.3 Unit matrix: each template kind in env values, each in labels values, unknown name in env, unterminated span in labels, span in label key, post-substitution validation intact, template-free profiles byte-identical.

## 2. Live regression

- [ ] 2.1 Add a live conduct asserting harness-observed env values resolve plus label round-trip with template-bearing values.
- [ ] 2.2 Verify the new test fails against the pre-change binary (literal `{{...}}` in observed env), and re-adopt `{{container-home}}` env values in the seat profile with a probe that prints every touched field.

## 3. Validation and review

- [ ] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through tier-1 review (Reviewer General) plus a QA Partner dogfood pass — no tier-2 (bug fix against specced fail-closed behavior, no boundary redesign) — then autosquash, sync, archive, and push on approval.
