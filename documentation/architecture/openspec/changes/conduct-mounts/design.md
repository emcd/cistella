## Context

`conduct_session` (`src/main.rs`) appends the worktree triple with a hardcoded `/work` target; mounts otherwise come only from profiles; `validate_mounts` (`src/mount.rs`) rejects any ancestor/descendant triple pair. Dogfood (host-path worktrees, per-project notebook mounts, RO-parent/RW-child notebook stacking) needs all three relaxed. Stakeholders: Agentmux (will supply pairs + per-project triples), PC Setup (image side unaffected).

## Goals / Non-Goals

**Goals:**
- Worktree mountable at its canonical host path with `/work` preserved as default.
- Per-invocation extra mounts without per-session profiles.
- RO-parent/RW-child stacking for notebook visibility.

**Non-Goals:**
- Changing `cistella.directory` semantics (stays canonical host dir) or session identity.
- Secret redaction in mounts (`cistella:todos/13` tracks that separately).
- Env-var-driven mount roots for Codex/Claude (profile `env` already covers; deferred).

## Decisions

- **Rename conduct's flag to `--session-directory` with hidden `--cwd` alias.** `--directory` is too bare next to `container_home` and the new container-target half; `--session-directory` names the concept precisely. The S1 `--cwd` rejection is reversed deliberately: abbreviations (`--cwd`) and portmanteaus (`--workdir`) are acceptable as *aliases*, while standalone truncations (`--config`, `--dir`, `--env`) stay out of both primary and alias vocabulary. `--cwd` over `--workdir`: conventional abbreviation for exactly this concept, and it echoes the omit-the-flag default (process cwd). Hidden alias keeps `--help` vocabulary clean. Alternative keep-`--directory` rejected: specificity matters once the flag carries two paths.
- **Rename is conduct-scoped; selectors keep `--directory`.** `enter`/`survey`/`inspect`/`terminate` filter by host path against `cistella.directory` — there is no container side to name, so no pair form and no rename. The two flags are different concepts sharing a word today; after this change each names its concept precisely. Alternative global rename rejected: touches runtime/transport specs and every selector fixture for zero behavior gain.

- **`<host>[:<container>]` single flag over a `ctn-`-prefixed pair.** One parsing/validation story shared with the mount-triple idiom; no inconsistent partial states (container target without host dir). `:`-in-path ambiguity accepted (Linux paths; the triple parser already lives with it). Alternative pair rejected: more surface for the same information.
- **CLI triples union with profile triples; exact-target CLI override wins, partial overlap errors.** An identical canonical target is unambiguous replace intent (mirrors the CLI-wins label rule); ancestor/descendant overlap outside the RO-ancestor rule is ambiguous (replace? stack? typo?) and fails closed. Mount ordering is not the concern — depth sort renders either way — intent-clarity is. Alternative blanket-error rejected: too strict for deliberate per-session mode/source swaps. Alternative blanket CLI-wins rejected: partial overlap would silently restack seat policy.
- **`--mount`, not `--volume`/`--bind`.** `--volume` collides with Docker's container-attached (non-host) volume concept; `--mount` matches the mounts-spec vocabulary. No compact shell-safe alternative to `host:target:mode` was found (flag triplets, comma forms, and JSON are all worse); the open syntax question is closed.
- **Relax overlap only for RO-ancestor/RW-descendant (or RO/RO).** Never WW nesting (two writers to overlapping trees is never an expressible intent) and never anything shadowing session-home. Depth-ordered rendering already mounts parents before children, so stacking works with no renderer change. Alternative blanket-allow rejected: reintroduces the masking bugs the rule was written for.
- **Defaults keep baked examples portable.** `/work` default, no CLI mounts required, no nesting required — `default`/`opencode` profiles behave byte-identically.

## Risks / Trade-offs

- **Host-path worktree leaks seat layout into the container** → Mitigation: opt-in per invocation/profile; baked examples stay `/work`.
- **`:` split on exotic paths** → Mitigation: split once on the first `:` (host side is an absolute path; a colon there fails absolute-path validation loudly). Document the limitation.
- **RW-child over RO-parent depends on mount ordering** → Mitigation: renderer already sorts by depth; add a regression test with the notebook shape.

## Migration Plan

- Profiles unchanged; conduct invocations using `--directory` break (Agentmux must move to `--session-directory`) — the BREAKING rename is the migration. New flags are additive otherwise. Rollback: revert; `--session-directory`/`--mount` stop existing and `--directory` returns.
- Dev profile adopts host-path worktree invocation plus RO `~/Dropbox/Notes` parent after landing.

## Open Questions

(none — syntax confirmed with `--mount host:target:mode`; observability follow-ups tracked in `cistella:todos/14`.)
