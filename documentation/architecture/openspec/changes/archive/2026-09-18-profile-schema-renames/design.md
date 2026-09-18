## Context

Profile deserialization lives in `src/profile.rs` (`Profile`, `MountTriple`); TOML key spellings are serde-level, Rust field names stay snake_case regardless. The fleet's profiles are few: two baked examples, the dev draft, the XDG `opencode` seat profile, and inline test fixtures. Seeding copies baked examples to XDG once and never overwrites, so pre-rename seeded files will fail to parse after the upgrade until the operator intervenes.

## Goals / Non-Goals

**Goals:**
- New spellings everywhere the driver reads or ships profiles, in one atomic change.
- One migration note covering both renames; every existing profile fixable by mechanical key renaming.

**Non-Goals:**
- No backward-compat shim for old spellings (rejected: permanent dual-schema surface for a young project with a countable fleet).
- No Rust-side renames; no behavior, validation, template, or CLI changes.
- No automatic migration of user XDG files (rejected: the driver never mutates user files; silent rewriting of config is worse than a loud parse error).

## Decisions

- **Serde `rename_all = "kebab-case"` on `Profile` and `MountTriple`, plus `#[serde(rename = "environment")]` on the env field.** Single attribute each, compiler-checked coverage of every key. Alternative considered: per-field `rename` attributes — equivalent but verbose and easy to miss one; `rename_all` is exhaustive by construction.
- **Fail closed on old spellings.** Unknown TOML keys are already parse errors through the existing struct definitions (verify `deny_unknown_fields` posture during implementation; if absent, the old keys fail as missing-required/ignored — either way no silent acceptance of a half-migrated profile, but the error quality differs and the implementer picks the better failure). No shim, no deprecation window.
- **Migration = mechanical rename + re-seed path documented.** Note covers: the key map (old → new), the re-seed escape hatch (delete seeded file, conduct re-seeds the new example), and the dogfood profiles as worked examples. The implementer updates baked examples, fixtures, dev draft, and the XDG seat profile, and proves it with the full suite (fixtures are the coverage).

## Risks / Trade-offs

- [Risk] Stale seeded XDG copies break post-upgrade conducts until renamed → Mitigation: the parse error names the offending content; the migration note leads with the re-seed one-liner. Fleet is countable (operator + QA).
- [Risk] A fixture missed in the rewrite silently keeps old spelling green → Mitigation: old spellings fail parse, so any missed fixture fails loudly in the suite, not silently.
- [Risk] External profiles (agentmux-side docs referencing keys) drift → Mitigation: grep the workspace for old spellings as a task step; docs updated in the same change.

## Migration Plan

Ship the rename; publish the migration note (in the delta spec); re-seed or hand-rename the two dogfood profiles as the worked proof; QA restarts on the new profile. Rollback is reverting the one change (old profiles parse again).

## Open Questions

None. The only genuine fork (shim vs. break) is decided: break once, loudly, with a note.
