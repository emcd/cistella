## 1. Unit emission

- [ ] 1.1 Emit `Init=true` in the `[Container]` section of `generate_quadlet_unit` (next to `UserNS=keep-id`) with a comment citing PID 1 SIGTERM forwarding and zombie reaping.
- [ ] 1.2 Add a unit assertion that the generated text contains `Init=true` inside the `[Container]` section.

## 2. Timing regression (live)

- [ ] 2.1 Add a live lifecycle test asserting the stop/teardown phase completes in under 5 seconds with no SIGKILL fallback, per the delta scenario.
- [ ] 2.2 Verify the new test fails against the pre-change unit (drop `Init=true` locally, observe the ~10 s stop) and passes with it.

## 3. Validation and review

- [ ] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through the standard two-tier review (Reviewer General tier-1, Advisor tier-2) plus a QA Partner dogfood pass, then autosquash, sync, archive, and push on approval.
