## 1. Promote the guest binary

- [x] 1.1 Move `tests/fixtures/tempdir_guest/main.rs` to `src/bin/cistella-extension-tmpdir.rs` (verbatim logic); change the `--role=` default from `tempdir` to `tmpdir`; drop the `[[example]] tempdir_guest` table (autobins picks up `src/bin/`).
- [x] 1.2 Switch tmpdir peer resolution from the cargo-examples glob to sibling-relative release binaries (keep newest-mtime selection semantics); keep the 755 normalization at resolution.
- [x] 1.3 Resolve `tmpdir` as shipped: hardcode beside `landlock` in `resolve_extensions` (`ExtensionResolution::Tmpdir`), add `ExtensionSpawn::Tmpdir` mirroring the landlock sibling spawn (install-dir trust, offer role + contribution set, require role); handle the new variant in the single + pair arms. V1 pair rule untouched (landlock+tmpdir is the admitted pair).

## 2. Supervisor rename

- [x] 2.1 Rename `src/bin/cistella-landlock-wrap.rs` to `src/bin/cistella-supervisor-landlock.rs`; update `WRAPPER_FILE_NAME` and `STAGED_WRAPPER_GUEST_PATH` (`/run/cistella/hooks/supervisor-landlock`); update docs and all test references (wrapper, mountprep, hook_live, prepare_hooks, modes_prepare).

## 3. tmpdir alignment (role, profiles, docs, tests)

- [x] 3.1 Rename role strings: hello default, fixture profiles (`tmpdir.toml`, `tmpdir-hooked.toml`), `profiles.md` (registry stanza → shipped note), unit test role assertions (guest_hosting via shipped binary). No registries anywhere (test or production).
- [x] 3.2 Rename the main spec directory `tempdir-service` → `tmpdir-service` (title, role strings, fixture-grade language → shipped-guest terms); delta `src-confinement` wordings (carveout body, admitted-slice scenario); add `guest-hosting` delta (shipped list + shipped-second scenario); verify standalone `tempdir` grep reads empty outside `archive/` and historical change-docs (`tempfile::TempDir` API stays).
- [x] 3.3 Maintainer canary profile edit DONE (2026-10-13, operator-authorized): `tmpdir` extension added, `TMPDIR`/`BUN_TMPDIR` dropped, TOML-validated. Re-conduct + observation is 4.2 (operator).

## 4. Validate and review

- [x] 4.1 Fast suite green under umask 002 and 022; clippy, fmt, `openspec validate --all --strict`; tier-1 review (planning + implementation + redirect; tier-2 out of scope per operator routing). QA: 620/620 at 1d8f731 (behavior) + 620/620 at 8546487 (shipped mechanism, four executed + hooked).
- [ ] 4.2 Operator prerequisites (outside this change, tracked here): install shipped binaries beside the driver → validate profile → re-conduct maintainer seat → observe stock-`/tmp` tooling, slice mint/reap → rollout or revert-file rollback.
- [x] 4.3 Rework live tmpdir tests to declare the extension with no registry (proving shipped resolution); unit test uses the `Tmpdir` spawn directly.
