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
  as its own `--profile` flag. Live must go green on host per hash
  before merge.
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
