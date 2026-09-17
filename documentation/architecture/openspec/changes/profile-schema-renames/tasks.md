## 1. Serde renames

- [ ] 1.1 Apply `rename_all = "kebab-case"` on `Profile` and `MountTriple` plus `rename = "environment"` on the env field in `src/profile.rs`; verify `deny_unknown_fields` posture and pick the better old-spelling failure.
- [ ] 1.2 Rewrite baked examples (`data/profiles/*.toml`) and every inline-TOML test fixture to the new spellings.

## 2. Fleet profiles and docs

- [ ] 2.1 Update the dev draft profile and the XDG `opencode` seat profile; grep the workspace for leftover old spellings (profiles, docs, READMEs) and update in the same change.
- [ ] 2.2 Prove behavior unchanged: full suite green with rewritten fixtures (old spellings fail parse — any missed fixture fails loudly).

## 3. Validation and review

- [ ] 3.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, the fast suite, the full live tier, and `openspec validate --all --strict`.
- [ ] 3.2 Send the implementation through tier-1 review (Reviewer General) plus a QA Partner dogfood pass (QA restarts on the renamed seat profile) — no tier-2 (mechanical rename, no behavior or boundary change) — then sync, archive, and push on approval.
