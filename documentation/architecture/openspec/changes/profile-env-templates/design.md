## Context

`expand_templates` in `src/profile.rs` runs a scan pass (`template_values` → `split_spans`, collecting span names for precedence-ordered errors) then a substitute pass over the same value set. That set is mount triples both sides plus command argv. `env` values and `labels` values skip both passes: no substitution, and no unknown/unterminated-span errors either. The seat profile's three `{{container-home}}` env values therefore arrived inside the container literally, breaking agentmux config resolution, the `TMUX` socket path, and cargo `PATH` (`cistella:issues/3`). The pipeline structure itself is sound; its coverage is not.

## Goals / Non-Goals

**Goals:**
- Uniform expansion rule across all container-facing free text, with fail-closed spans everywhere else.
- Keep every existing guarantee: precedence-ordered errors, no-rescan, post-substitution validation, template-free laziness.

**Non-Goals:**
- No new template names or syntax.
- No change to `container_home` (rejected pre-canonicalization), `HOME` derivation, env key charset, label key rules, or CLI `--label` handling.
- No transport/runtime/registry changes.

## Decisions

- **Expand `env` values and `labels` table values in the existing two passes** (add them to `template_values` and to the substitution loop). Alternative considered: env-only, leaving labels literal — rejected because it preserves a per-field matrix authors must memorize, and a `{{project-name}}` label value (e.g. session tagging) is as legitimate as a template env value. One rule: container-facing text expands.
- **Spans in label keys are typed errors, never expanded.** Expanding keys would destabilize label matching and registry lookups (keys are lookup dimensions, values are payload). Env keys need no handling: `[A-Z_][A-Z0-9_]*` cannot contain braces, so they are inherently template-free.
- **Substitute before existing value validations.** `env` values currently reject `=`/`\n`; labels values reject `=`/`\n`/`\0` plus the `cistella.` prefix. Running those checks on post-substitution text keeps the guarantees on what actually reaches the unit. All three template value domains (container-home path, host-home path, validated project-name charset) are clean under these rules, so no previously-valid profile changes outcome — except profiles containing spans, which previously passed silently and now either expand or error. That behavior change is the fix.
- **Live coverage asserts harness-observed values.** A conduct with template-bearing env prints its environment; the test asserts resolved values (the exact assertion the original verification skipped). Labels assert via the existing label round-trip helper.

## Risks / Trade-offs

- [Risk] A profile in the wild already contains a literal `{{...}}` in env/labels that "worked" (silently wrong) → Mitigation: it was never correct — the value reached the container literally; erroring is strictly more honest, and the only known template-bearing profile is the seat profile (reverted to literals, re-adopts after landing).
- [Risk] `{{project-name}}` in labels changes label cardinality per project → Mitigation: author opt-in per value; matching semantics untouched.
- [Risk] Test-matrix growth on an already slow live tier → Mitigation: one live conduct covers env + labels together; the matrix lives in unit tests (sub-second).

## Migration Plan

None. Template-free profiles are byte-identical through the wider passes (no spans → no changes). The seat profile re-adopts `{{container-home}}` env values after landing, verified by a probe that prints every touched field.

## Open Questions

None. Scope (env + labels values, fail-closed keys) follows directly from the uniform-rule decision.
