## Context

The template engine (`src/profile.rs`: `split_spans` → name collection with precedence-ordered errors → lazy value resolution → single-pass `substitute`, no rescan) currently knows three flat names. The seat profiles hardcode `bundles/infrastructure` in TMUX because no caller-provided channel exists, and `container-home` is a literal per-host path because host env is unreachable. CLI surface for conduct: `--profile`, `--session-directory`, `--project-name`, `--mount`, labels; supplements join that family.

## Goals / Non-Goals

**Goals:**
- Provenance-visible templates: every span's context readable at a glance.
- Agentmux (and future callers) pass per-invocation data without profile edits.
- Portable example profiles (`container-home` from host env).

**Non-Goals:**
- No new expansion fields beyond the `container-home` early phase (four standard value classes plus the pre-resolution phase).
- No change to ordering/precedence/laziness/no-rescan semantics — namespaces slot into the existing pipeline.
- No `environment:`-to-container passthrough of sensitive names (explicit denylist).

## Decisions

- **`:` separator, `core:` / `supplement:` / `environment:` contexts.** `:` reads as namespace (consistent with `cistella.*` labels) and avoids dotted-path confusion. Bare `{{name}}` (no context) becomes a typed error directing to qualified spellings — no silent grandfathering, per the pre-1.0 sharp-break agreement.
- **`--supplement k=v`, repeatable, conduct-only, last-wins on duplicates.** Keys `[A-Za-z0-9_. -]+`-ish charset (exact class pinned in implementation, conservative), non-empty, never `cistella` nor beginning with `cistella.`, and valid TOML scalar identifiers to keep diagnostics sane; values are opaque until substitution — no generic project-name gate on the raw value, so a supplement may supply a whole path; each fully substituted sink applies its existing validator (`container-home` absolute/canonical/sensitive-root, mounts topology/injection, labels/env rules, argv controls) — validation happens post-substitution through the existing gates, so no new validation code paths, only new value sources. Callers should pass stable values across invocations (the agentmux caller-side shape is the model). Supplements are trusted, non-secret caller metadata: no credential-name denial is applied to supplement values — the conduct caller already controls profile, mounts, labels, and harness argv, so a supplement deny would add no security and would be bypassable by renaming. Callers must not bridge ambient secrets into supplement values.
- **`environment:` is allowlist-by-default with a hard credential deny on top, both fixed at compile time.** Only referenced names are looked up (no env dump). The allowlist is a code-reviewed constant, not runtime configuration: initial contents `HOME` alone (the sole concrete consumer is the `container-home` showcase; `USER`/`TERM`/`LANG`/`HOSTNAME` stay off until a profile needs them, and `EDITOR` stays off as executable-policy input rather than ambient data). There is deliberately no runtime/env/CLI extension knob in 0.1.0 — any extension is a code change with review, because a same-invocation extension would collapse allowlist-by-default for novel credential names the patterns miss (`DATABASE_URL`, auth cookies, vendor names). Credential-shaped names are hard-refused even if allowlisted, matched case-insensitively (exact `SSH_AUTH_SOCK` plus `*_TOKEN`, `*_SECRET`, `*_KEY`, `*_PASSWORD`, `*_CREDENTIAL*` — exact patterns pinned in implementation, erring broad). Fixed ordering: hard-deny check, then allowlist, then `std::env` lookup. Diagnostics name only the variable, never its value; allowlist-absent ("not allowlisted") and hard-refusal diagnostics are distinct so the operator knows a typo/opt-in request from a refusal. Fail-closed direction preserved: unknown sensitivity → refuse.
- **`container-home` expands early as a fifth sink (Option B).** Before the standard pipeline runs, `container-home` resolves allowlisted `{{environment:*}}` and supplied `{{supplement:*}}` spans; then canonicalization and sensitive-root validation run on the substituted value exactly as they do today for literals; then `core:container-home` is constructed from the validated result. `{{core:*}}` spans inside `container-home` are typed cycle errors (self-reference: `core:container-home` derives from this very field). No-rescan applies to the substituted value. The pre-existing template-free literal path is the degenerate case of this phase (no spans → straight to canonicalization), so current profiles behave identically.
- **`project-name` reparents to `core:project-name` with `--project-name` unchanged.** The flag is CLI surface (stable); only the span spelling moves. Same for the other two natives.
- **Seat profiles migrate in the same change** (`container-home = '{{environment:HOME}}'` showcase in baked examples; TMUX via `{{supplement:bundle-name}}`), with agentmux coder entries updated as a companion (their repo, coordinated, not blocking: old hardcoded profiles keep working until they pass supplements — no, sharp break: coder entries must pass the names or conduct fails closed with "unknown supplement". Flagged as coordinated landing.)

## Risks / Trade-offs

- [Risk] `environment:` varies conduct by host env (reproducibility) → Mitigation: only referenced names read; resolution failures are loud; profiles should prefer `core:`/`supplement:` for determinism. Documented guidance, not enforcement.
- [Risk] Denylist incompleteness (novel credential env names) → Mitigation: err broad with pattern rules (`*_TOKEN` etc.) plus exact names; reviewers own additions. Fail-closed direction: unknown sensitivity → refuse.
- [Risk] Agentmux rollout skew (code passing supplements vs profiles requiring them) → Mitigation: coordinated landing; typed errors name the missing supplement key, so misconfiguration is diagnosable, not silent.
- [Risk] Breaking all existing template profiles at once → Mitigation: pre-1.0, fleet is two seats + examples; migration is mechanical (prefix `core:`), done in the same change.

## Migration Plan

Land with migrated seats/examples/coders; no compat shim. Rollback is reverting the one change.

## Open Questions

- Exact `supplement` key charset (conservative ASCII subset; pinned in implementation).
- Exact code-level allowlist/deny tables (from the proposal lists: allow `{HOME}`; deny exact `SSH_AUTH_SOCK` + suffix patterns, case-insensitive; deny-before-allowlist-before-lookup ordering).
