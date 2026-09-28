# Maintainer guide

Image builds, validation tiers, and commit gates for cistella itself.
User-facing setup lives under `documentation/usage/`; the trust model
(ambient authority, known caveats, planned work) is documented at
`documentation/usage/trust-model.md` and applies here too.

## Session images

Per-harness, self-contained examples under `data/dockerfiles/` — not
published bases. Build and verify from the repo root:

```sh
./data/dockerfiles/validate.sh
```

This builds each example image and verifies baked terminfo (no mount,
no ambient `TERMINFO`) plus harness binary discovery. Baked terminfo
entries live in `data/terminfos/`. Production seat images are owned by
the host team; toolchain additions (clipboard tools, linters, hook
binaries such as `linecheck`) go through them, not through this repo.

## Validation tiers

- Fast (default, hermetic): `cargo nextest run --config-file
  .auxiliary/configuration/nextest.toml`. Live tests are `#[ignore]`
  and pass vacuously without a systemd user manager plus Podman.
- Full live tier (host only — seats lack Podman/systemd):
  ```sh
  cargo nextest run --config-file .auxiliary/configuration/nextest.toml -P live --run-ignored=all
  ```
  Flag order matters: without `--config-file`, cargo misreads `-P live`
  as its own `--profile` flag. On-demand runs and the nightly
  schedule (planned, not yet implemented) use the full tier;
  per-merge requirement is the smoke gate below, and a green
  skip never counts as proof.
- Live-smoke gate (tester/pre-push, ≤10 min budget): the
  `smoke` rows of `tests/tiers.txt` (one `<lowest-tier>
  <test-id>` row per live test; membership cumulative
  downward), run as
  ```sh
  cargo nextest run --config-file .auxiliary/configuration/nextest.toml -P live --run-ignored=all -E "$(.auxiliary/configuration/live-smoke-filter.sh)"
  ```
  The script emits exact-equality predicates (`test(=id)` —
  bare `test(id)` is contains-match and would silently
  widen). Everything else stays in-tree and runnable
  (nightly/on-demand full tier is planned, not yet
  scheduled); per-merge requirement is the smoke gate plus,
  on security-sensitive releases, explicitly scheduled
  targeted evidence (e.g. §3.4 denial/adversarial-bind
  runs) — a green skip never counts as proof.
- Specs: `openspec validate --all --strict`.
- Lint/format/size: `cargo clippy --all-targets -- -D warnings`,
  `cargo fmt --check`, and the `linecheck` hook
  (`.auxiliary/configuration/linecheck.yaml`; Rust sources must stay
  under the per-file error threshold — split modules instead of
  growing them).

## Commit gates

Pre-commit hooks run fmt, clippy, linecheck, and the fast suite;
pre-push runs the release build plus the live tier. A hook rejection
means no commit was created: fix the finding, restage, rerun the same
command — never amend around a failed commit. Commits use present
tense, imperative mood, and end with a `Co-Authored-By` trailer.
Review findings become `fixup!` commits (autosquash only after
reviewer approval); merge and push require explicit human approval.
