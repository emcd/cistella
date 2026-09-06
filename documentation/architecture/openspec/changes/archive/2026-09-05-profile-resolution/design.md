## Context

`Profile::resolve` (`src/profile.rs`) implements name lookup as cwd-relative `data/profiles/<name>.toml` with a `CARGO_MANIFEST_DIR` fallback. Both legs assume a source checkout. The mounts spec requires "shipped `data/profiles/<name>.toml` resolved by name or path" without saying where shipped files live at runtime. Agentmux (`~/src/agentmux`) already solved this shape: templates/examples baked with `include_str!`, dropped into the XDG standard location when absent. Stakeholders: driver users (dogfood first), PC Setup (image/profile ownership boundary stays unchanged — profiles stay user-side).

## Goals / Non-Goals

**Goals:**
- A name resolves identically from any cwd on any machine.
- Examples are always available (baked) yet always customizable (seeded copies).
- Development profiles never leak into the shipped set.

**Non-Goals:**
- `cistella init` questionnaire and template gallery (`todos/profiles/1`) — this change only fixes lookup and seeding; authoring UX follows.
- Registry/download of profiles from anywhere networked.
- Migrating existing user files (none exist in the wild yet).

## Decisions

- **Order: explicit path → configuration directory → XDG user → baked examples.** Explicit paths bypass everything (scripting escape hatch). A configuration directory (`--configuration-directory`, then `$CISTELLA_CONFIGURATION_DIRECTORY`) names `<dir>/profiles/<name>.toml` and is a closed tier following the Agentmux roots model: no fallthrough to XDG or baked, a missing name is a typed error, and supplied tiers are never seeded — so a typo'd custom name can never silently resolve to a baked example. XDG beats baked so customization sticks. No magic development-directory detection of any kind (no marker inspection, no worktree logic): development profiles live wherever the developer wants and are passed explicitly, keeping local configuration out of the code tree. Alternative cwd-anchored dev dir rejected after review: it needed a non-baked-names guard to stay stable under seeding and remained an ambient config source.
- **Seed-if-absent, per file, only on reaching the default tier.** When a named lookup passes the supplied tiers and reaches XDG/baked, `resolve` creates the XDG dir and copies each missing baked example; user edits are never overwritten; explicit paths and supplied configuration-directory tiers never trigger seeding (Agentmux `starter.rs` rule: never answer "you named a layer that is not there" with fresh scaffolding). Unlike Agentmux bundles (union semantics — seeding beside user bundles would add a live bundle), profiles are inert until named, so seeding missing baked names beside user files is safe. Writes use `create_new(true)` with `AlreadyExists` tolerated (atomic no-clobber under races). Seeding lives in `resolve` (not a separate command) because `conduct` is the sole profile consumer — no new verb, no separate step to forget; the seeder is written so a future `cistella init` (`profiles/1`) can reuse it.
- **Bake with `include_str!`, not runtime file reads.** Per-template `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/profiles/<name>.toml"))`, exactly as Agentmux `starter.rs` does for its configuration templates. The binary carries the examples at compile time; the source dir remains the single place to edit them. Alternative runtime-relative-to-exe lookup rejected: fragile across install layouts (cargo, deb, nix).
- **Bake with `include_str!`, not runtime file reads.** The binary carries `data/profiles/*.toml` at compile time; the source dir remains the single place to edit examples. Alternative runtime-relative-to-exe lookup rejected: fragile across install layouts (cargo, deb, nix).
- **Keep the `.toml`-suffix/path heuristic for explicit paths.** Unchanged behavior; only the name branch changes.

## Risks / Trade-offs

- **Stale seeded copies** → Mitigation: seed-if-absent never overwrites; a `cistella doctor`-style staleness hint is future work, not this change.
- **`XDG_CONFIG_HOME` unset/empty** → Mitigation: default `~/.config`, same fallback already used for scratch/lock paths.

## Migration Plan

- No data migration: no user deployments exist. `data/profiles/{default,opencode}.toml` stay byte-identical (now documented as examples); the dogfood draft lives at `.auxiliary/configuration/cistella/profiles/cistella-dev.toml`, uncommitted per-host local config.
- Rollback: revert the resolve rewrite; explicit `--profile <path>` keeps working throughout.

## Open Questions

- Should `survey`/`inspect` show which tier a profile resolved from (explicit path, supplied configuration directory, XDG, baked)? Useful for debugging hierarchical lookup; deferred to follow-up UX work.
