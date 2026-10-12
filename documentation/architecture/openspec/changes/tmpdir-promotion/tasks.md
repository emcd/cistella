## 1. Promote the guest binary

- [x] 1.1 Move `tests/fixtures/tempdir_guest/main.rs` to `src/bin/cistella-extension-tmpdir.rs` (verbatim logic); change the `--role=` default from `tempdir` to `tmpdir`; drop the `[[example]] tempdir_guest` table (autobins picks up `src/bin/`).
- [x] 1.2 Switch tempdir peer resolution from the cargo-examples glob to sibling-relative release binaries (keep newest-mtime selection semantics); keep the 755 normalization at resolution.

## 2. Supervisor rename

- [x] 2.1 Rename `src/bin/cistella-landlock-wrap.rs` to `src/bin/cistella-supervisor-landlock.rs`; update `WRAPPER_FILE_NAME` and `STAGED_WRAPPER_GUEST_PATH` (`/run/cistella/hooks/supervisor-landlock`); update docs and all test references (wrapper, mountprep, hook_live, prepare_hooks, modes_prepare).

## 3. tmpdir alignment (role, profiles, docs, tests)

- [x] 3.1 Rename role strings: registry entry names (`tempdir-test` → operator-confirmed production name, default `tmpdir`), fixture profiles (`tempdir.toml`, `tempdir-hooked.toml`), `profiles.md`, test registries and role assertions (guest_hosting, tempdir_gc).
- [x] 3.2 Rename the main spec directory `tempdir-service` → `tmpdir-service` (title, role strings, fixture-grade language → shipped-guest terms); delta `src-confinement` wordings (carveout body, admitted-slice scenario); verify `tempdir` grep reads empty outside `archive/`.
- [ ] 3.3 Maintainer canary profile edit (HELD until the operator registry entry exists — unknown extensions refuse pre-create, so this edit is live, not inert): add the `tmpdir` extension block, drop `TMPDIR`/`BUN_TMPDIR` overrides.

## 4. Validate and review

- [ ] 4.1 Fast suite green under umask 002 and 022; clippy, fmt, `openspec validate --all --strict`; dual review (tier-1 + tier-2, renames are trust-surface: staged paths, registry roles, peer resolution).
- [ ] 4.2 Operator prerequisites (outside this change, tracked here): host registry entry with pinned digest → validate profile → re-conduct maintainer seat → observe stock-`/tmp` tooling, slice mint/reap → rollout or revert-file rollback.
