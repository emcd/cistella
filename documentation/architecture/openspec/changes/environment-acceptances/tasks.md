## 1. Schema and parsing

- [ ] 1.1 Rename `profile.environment` to assignments (`[environment-assignments]`) with `deny_unknown_fields` turning legacy `[environment]` into a typed parse error; add top-level `environment-acceptances: Vec<String>` (default empty).
- [ ] 1.2 Validate acceptance names (env-name grammar, duplicate rejection in profile-list order, deterministic first error); no deny/allowlist checks on this path. Document the reserved union direction (list-or-table, list ≡ defaults, list valid forever) in the field rustdoc.

## 2. Resolution

- [ ] 2.1 Snapshot all accepted values from invoker env up front in profile-list order; absent name fails before any session/runtime mutation with a name-only diagnostic (no unit/scratch/container residue; XDG lookup seeding excluded).
- [ ] 2.2 Collision errors: acceptance vs assignment keys and vs driver-owned/implicit names (`HOME` always; credential-surface-injected names enumerated from that path); no shadowing either way.
- [ ] 2.3 Insert accepted pairs post-substitution (no template parse/rescan) and render through the shared `Environment=` unit path behind the value gate (reject non-Unicode/line-breaking values value-free; `=` permitted). Map non-Unicode `VarError` without rendering the contained `OsString`; verify semantic value equality in-container post-escaping, not unit syntax.

## 3. Tests

- [ ] 3.1 Unit matrix: declaration validation (grammar/dupes/secret-shaped permitted), requiredness (absent fails residue-free, list-order first error), collisions (assignment/`HOME`/injected), verbatim (`{{...}}` literal, `=` intact, CR/LF refused value-free), no-render-permission (accepted name in a span still fails closed); shared-gate parity (assignments and acceptances reject the same line-break class, both permit ordinary `=`; matrix also pins assignment NUL rejection per the requirement even though accepted OS values cannot contain NUL).
- [ ] 3.2 Live conduct proving invoker-env forwarding end to end (relay-style vars visible in container env).

## 4. Migration and docs

- [ ] 4.1 Migrate host seat profiles and baked example(s); update `documentation/usage/profiles.md` and trust-model note (accepted secrets conveyed transiently in unit files, never displayed — phrase concretely: diagnostics carry names only; values appear in the unit `Environment=` lines for the session lifetime, torn down with the session; also note `[environment-assignments]` CR/LF rejection as new in 0.1.1).
- [ ] 4.2 Validate: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, fast suite plus live tier on host.
