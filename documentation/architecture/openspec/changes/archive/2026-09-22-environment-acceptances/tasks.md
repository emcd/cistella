## 1. Schema and parsing

- [x] 1.1 Rename `profile.environment` to assignments (`[environment-assignments]`) with `deny_unknown_fields` turning legacy `[environment]` into a typed parse error; add top-level `environment-acceptances: Vec<String>` (default empty).
- [x] 1.2 Validate acceptance names (env-name grammar, duplicate rejection in profile-list order, deterministic first error); no deny/allowlist checks on this path. Document the reserved union direction (list-or-table, list ≡ defaults, list valid forever) in the field rustdoc.

## 2. Resolution

- [x] 2.1 Snapshot all accepted values from invoker env up front in profile-list order
- [x] 2.2 Collision errors: acceptance vs assignment keys and vs driver-owned/implicit names
- [x] 2.3 Insert accepted pairs post-substitution (no template parse/rescan)

## 3. Tests

- [x] 3.1 Unit matrix: declaration validation (grammar/dupes/secret-shaped permitted)
- [x] 3.2 Live conduct proving invoker-env forwarding end to end (`environment_acceptances_forward_live`; host-verified in the 165-test live tier).

## 4. Migration and docs

- [x] 4.1 Migrate host seat profiles and baked example(s)
- [x] 4.2 Validate: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, fast suite plus live tier on host (fast 133/133 in-seat; live 165/165 host-verified).
