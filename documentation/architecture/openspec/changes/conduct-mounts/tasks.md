## 1. Directory pair form (conduct only; selectors keep `--directory`)

- [x] 1.1 Rename conduct's `--directory` to `--session-directory` with hidden `--cwd` alias (clap `alias`, not `visible_alias`); selector flags and their fixtures are untouched
- [x] 1.2 Extend `--session-directory` to `<host>[:<container>]` (split once on first `:`, container defaults to `/work`); reject empty sides and non-absolute targets with typed errors
- [x] 1.3 Thread the target through `conduct_session` (replacing the hardcoded `/work` triple target); `cistella.directory` keeps recording the canonical host dir
- [x] 1.4 Update conduct `--directory` fixtures (tests, helpers, docs) to `--session-directory`, plus one alias-exercise test; selector `--directory` fixtures stay as-is

## 2. CLI mount triples

- [x] 2.1 Add repeatable `--mount <host>:<target>:<mode>` (conduct only), reusing the profile triple parser (`ro`/`rw`, same charset/injection rules)
- [x] 2.2 Union CLI triples with profile triples pre-validation; exact canonical-target match overrides (CLI wins), ancestor/descendant CLI/profile overlap outside the RO-ancestor rule is a typed error; duplicate `--mount` same target and `--mount` on the worktree target are typed errors

## 3. Nested-mount relaxation

- [x] 3.1 Relax `validate_mounts` overlap rule: allow descendant over read-only ancestor (forbid WW nesting and session-home shadowing as before); depth ordering already mounts parents first — add a regression test with the RO-parent/RW-child notebook shape
- [x] 3.2 Add resolution/unit tests: pair-form default and explicit target, CLI union, exact-target CLI override, partial CLI/profile overlap refusal, WW-nesting refusal, `:`-in-path documented limitation

## 4. Validation

- [x] 4.1 Run `cargo clippy --all-targets -- -D warnings` and `cargo nextest run --config-file .auxiliary/configuration/nextest.toml` green (fast + live); `openspec validate --all --strict` green
- [x] 4.2 Adopt in dogfood profile: host-path worktree invocation plus RO `~/Dropbox/Notes` parent triple; verify notebook read/write split live
