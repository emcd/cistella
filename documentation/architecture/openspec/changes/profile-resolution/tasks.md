## 1. Baked examples and lookup rewrite

- [x] 1.1 Bake `data/profiles/*.toml` with `include_str!` and document the dir as examples (README line + profile.rs docs)
- [x] 1.2 Add `--configuration-directory <dir>` (conduct only) and `$CISTELLA_CONFIGURATION_DIRECTORY` tiers naming `<dir>/profiles/<name>.toml` as closed lists (missing name errors, no fallthrough, never seeded); then XDG (`$XDG_CONFIG_HOME/cistella/profiles`, default `~/.config`); then baked map; delete cwd `data/profiles` and `CARGO_MANIFEST_DIR` legs and any dev-dir detection
- [x] 1.3 Seed-if-absent on default-tier name resolution only: `resolve` creates the XDG dir and copies missing baked examples with `create_new` (never overwrite, tolerate `AlreadyExists`); explicit missing paths error without creating anything

## 2. Dev profile and fixtures

- [x] 2.1 Dogfood profile lives at `.auxiliary/configuration/cistella/profiles/cistella-dev.toml` (uncommitted per-host local config, already moved; keep it out of git via `.auxiliary/.gitignore`)
- [x] 2.2 Update unit/integration fixtures that relied on cwd-relative `default` (point at baked name, or exercise `--configuration-directory` / environment selection where appropriate)
- [x] 2.3 Add resolution tests: XDG win over baked, supplied-directory precedence (flag over env over XDG, closed on missing, nothing seeded, no XDG writes), env tier, seed-if-absent leaves user files alone, explicit path bypass with stem label, explicit missing path errors with no scaffolding, name from foreign cwd

## 3. Validation

- [x] 3.1 Run `cargo clippy --all-targets -- -D warnings` and `cargo nextest run --config-file .auxiliary/configuration/nextest.toml` green; `openspec validate --all --strict` green
