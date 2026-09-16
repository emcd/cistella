## Context

`Profile::from_toml` (`src/profile.rs`) parses, `~`-expands, and validates in one pass; `resolve_in` calls it with no session context. Mount triples carry literal `/home/cistella/...` targets and `~`-prefixed host sources. Agentmux 0.10.2 (`session-template-composition`) interpolates `{{project-name}}` with lazy precedence (explicit override, `project-name-from = "bundle-name"`, else session-directory basename) and never rescans substituted bytes. Stakeholders: Agentmux (coder templates gain a receiver), dogfood seats (notebook mounts stop needing per-session flags).

## Goals / Non-Goals

**Goals:**
- No literal home paths in shipped or seat profiles; project-keyed mounts without per-session flags.
- Agentmux `--project-name {{project-name}}` works end to end with zero configuration (defaults agree).
- Expanded values face identical validation (no new bypass surface).

**Non-Goals:**
- Key renames (`todos/profiles/4`, `todos/profiles/5`) — separate breaking change, landed together later.
- Recursive/nested templates, templates in `image`/`labels`/`env keys` (mounts + command argv only).
- Changing `~` handling (stays, runs before template expansion so `~/x` keeps working).

## Decisions

- **Split parse → normalize-home → expand → validate.** `from_toml` keeps parsing + `~` + validation for literal profiles (existing callers/tests untouched); resolution gains home normalization (default-if-empty, template rejection, canonicalization) plus expansion between parse and validate. `{{container-home}}` is therefore always the canonical home — a traversing literal can never leak into expanded text. Validation still sees final values throughout.
- **`{{project-name}}` default = basename of canonical session directory; `--project-name` overrides.** Matches agentmux's default exactly, so both sides agree with no flags on either. Conduct computes the name once (after directory canonicalization) and threads it into resolution. Precedence inside cistella is flag-over-default only — agentmux's `project-name-from` variants compose upstream of what we receive. Alternative default (bundle/session id) rejected: cistella has no bundle concept; basename is what notebook paths key on.
- **Project-name charset restricted (alphanumeric plus `-_.`), validated before expansion.** Mirrors agentmux `validate_project_name`; a hostile name cannot smuggle `/`, `..`, or `=` past canonicalization — and defense in depth means rejecting it at the boundary anyway. Validation is lazy: only profiles that actually expand a `{{project-name}}` span consult the name, so template-free sessions in spaced directories keep working. Unknown `{{name}}` spans are typed errors naming the template, regardless of context.
- **Single-pass, no rescan.** Substituted values are never re-scanned for templates (agentmux precedent). A project named `{{container-home}}` stays literal text.
- **`{{host-home}}` reads `$HOME` at conduct time; missing `HOME` is a typed error.** Same source the `~` expansion already uses. `{{container-home}}` is the profile's own canonical `container_home` (post-canonicalization, so `/home/cistella/../x` forms cannot leak through).
- **`{{...}}` in `container_home` itself is a typed error.** Expansion needs the canonical home first (it defines `{{container-home}}`) — self-reference with no bootstrapping order. `HOME` derivation must stay static. Rejection runs before canonicalization/derivation (a `/home/{{host-home}}` value passes the absolute-path check and must never seed derivation). Alternative allow-with-cycle-protection rejected: no legitimate use (home roots are seat constants, not project-relative).
- **Resolution context is explicit; conduct owns the default.** `resolve_in` gains a project-name argument (`Option<ProjectName>` with `Explicit`/`DirectoryDefault` variants); conduct passes the flag or the canonical directory without deriving anything. `DirectoryDefault` derives the basename lazily at expansion — template-free sessions never consult it (root directory included). Context-free callers pass `None`: literal-or-error, never cwd-derived. Alternative eager defaulting rejected: it fails template-free root sessions for no reason.
- **Adopt in shipped example.** `data/profiles/opencode.toml` converts its four `/home/cistella/...` targets to `{{container-home}}/...`, proving templates in baked examples and keeping the seed text current. The seat `opencode` profile does NOT gain a `Notes/{{project-name}}` child triple yet: `nb` requires writes at the collection root, so the parent stays RW — and an RW child over an RW parent is WW nesting (correctly rejected). The child triple lands when the nb root-write need is understood (profile comment tracks it); until then `{{project-name}}` proves itself in unit/live tests and agentmux passthrough.

## Risks / Trade-offs

- **Template in baked example changes seeded text for new seats only** → Mitigation: seed-if-absent never overwrites; existing XDG copies keep literals, which still validate. No migration.
- **Project name with path separators rejected, not sanitized** → Mitigation: typed error at startup (fail closed); sanitizing (`..` → `_`) would silently mount the wrong tree.
- **`--project-name` + `--session-directory` basename disagree silently** → Mitigation: documented precedence (flag wins); the common case (both default) agrees by construction.

## Migration Plan

- Additive + one default behavior: profiles without templates behave byte-identically (expansion is a no-op pass). `{{project-name}}` defaults into every resolution — profiles not using it are unaffected. Rollback: revert; `--project-name` stops existing, templates become typed errors again (profiles using them fail closed, loudly).

## Open Questions

- Should `survey`/`inspect` show the expanded triples or the raw templates alongside the resolution tier (`todos/14`)? Deferred there — no new question, noting the coupling.
