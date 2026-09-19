## 1. Namespace engine

- [ ] 1.1 Extend the span parser to `context:name` with `:` separator; bare `{{name}}` becomes a typed error directing to qualified spellings. Unknown contexts and unknown names distinguished in diagnostics.
- [ ] 1.2 Add `--supplement k=v` (repeatable, conduct-only, last-wins) through conduct into resolution; supplement value table joins the lazy resolution with use-site validation via existing gates.
- [ ] 1.3 Add `environment:` lazy host-env lookup with a compile-time fixed allowlist (`HOME` only initially), case-insensitive credential deny checked before allowlist before `std::env` lookup, allowlist-absent vs hard-refusal diagnostics naming only the variable, and a boundary-case matrix (exact `SSH_AUTH_SOCK`, suffix/infix patterns, allowlisted-but-denied impossible-by-construction, absent-allowlisted, unallowlisted-present).
- [ ] 1.4 Reparent natives to `core:` (`container-home`, `host-home`, `project-name`); `--project-name` flag unchanged.
- [ ] 1.5 Add the `container-home` early expansion phase (allowlisted `environment:` + supplied `supplement:`, `core:` cycles rejected, canonicalization + sensitive-root validation on the substituted value, no-rescan) with normalization-parity (literal vs early-expanded safe traversal), control/root/cycle tests.

## 2. Profiles, examples, callers

- [ ] 2.1 Migrate baked examples (showcase `container-home = '{{environment:HOME}}'`) and both seat profiles (`{{supplement:bundle-name}}` in TMUX paths); coordinate agentmux coder entries passing `bundle-name` (companion change, coordinated landing).
- [ ] 2.2 Unit matrix per namespace (expand, unknown, missing, denylisted, bare-span errors) + live conduct with supplement and environment coverage.

## 3. Validation and review

- [ ] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through tier-1 review (Reviewer General) plus a QA Partner test-focus heads-up — tier-2 only if tier-1 flags boundary concerns (environment exfiltration surface is the watch area) — then sync, archive, and push on approval.
