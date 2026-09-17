## 1. Unit emission

- [x] 1.1 Emit `RunInit=true` in the `[Container]` section of `generate_quadlet_unit` (next to `UserNS=keep-id`) with a comment citing PID 1 SIGTERM forwarding and zombie reaping.
- [x] 1.2 Add a unit assertion that the generated text contains `RunInit=true` inside the `[Container]` section.

## 2. Timing regression (live)

- [x] 2.1 Add a live lifecycle test asserting the stop/teardown phase completes in under 5 seconds with no SIGKILL fallback, per the delta scenario.
- [x] 2.2 Verify the new test fails against the pre-change unit (drop `RunInit=true` locally, observe the ~10 s stop) and passes with it.

## 3. Validation and review

- [x] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through the standard two-tier review (Reviewer General tier-1, Advisor tier-2) plus a QA Partner dogfood pass, then autosquash, sync, archive, and push on approval.
