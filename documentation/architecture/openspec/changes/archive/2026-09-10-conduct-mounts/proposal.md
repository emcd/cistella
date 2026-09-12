## Why

Dogfood requires sessions that mirror the seat: the worktree mounted at its canonical host path (Git worktree pointers, filesystem remotes, and path-keyed harnesses all break under a fixed `/work`), per-project extra mounts (notebook repos differ per session), and stacked notebook visibility (project repo RW over an RO notebooks root). Today the worktree target is hardcoded to `/work`, mounts come only from profiles, and overlapping triples are unconditionally rejected — so none of these work.

## What Changes

- Rename conduct's `--directory` to `--session-directory` (reversing the S1 "no `--cwd` alias" decision: `--cwd` returns as a hidden alias only; the primary vocabulary stays fully spelled out, per the no-truncation rule). **BREAKING** for the one consumer (Agentmux) and any scripts; pre-dogfood, so cheap now, never cheap later. Scoped to conduct: `enter`/`survey`/`inspect`/`terminate` keep selecting by host-path `--directory` (a filter, not a mount target — no pair form, no rename), so runtime/transport specs are untouched.
- `--session-directory` accepts `<host>[:<container>]`: the session worktree mounts at the given container target, defaulting to `/work` when the container side is omitted. Agentmux passes the full pair; interactive use keeps the terse form.
- New repeatable `--label`-style CLI flag `--mount <host>:<target>:<mode>` (conduct only): extra triples unioned with the profile's mounts, subject to the same validation. A CLI triple with the exact canonical target of a profile triple overrides it (unambiguous replace intent, mirroring the CLI-wins label rule); ancestor/descendant CLI/profile overlap outside the RO-ancestor rule is a typed error (ambiguous intent, fail closed).
- Mount validation relaxes the overlap rule: a descendant triple over an RO ancestor is allowed (deepest mount wins; depth-ordered rendering already stacks these). All other overlaps stay rejected, as does shadowing session-home.

## Capabilities

### New Capabilities

(none — all changes modify the existing mounts contract)

### Modified Capabilities

- `mounts`: `--directory` renamed to `--session-directory` (hidden `--cwd` alias), worktree target configurable via its pair form; CLI-supplied triples as a second allowlist source with identical validation; nested RO-ancestor/RW-descendant stacking allowed with new scenarios.

## Impact

- `src/cli.rs` (two flags, conduct only), `src/main.rs` (`conduct_session` triple assembly), `src/mount.rs` (overlap rule), profile docs (dev profile adopts host-path worktree + RO notebooks parent once shipped).
- Agentmux (consumer): supplies `<host>:<container>` pairs and per-project `--mount` triples instead of per-session profiles.
- No registry/transport/identity changes: `cistella.directory` stays the canonical host dir, session ids and labels untouched.
