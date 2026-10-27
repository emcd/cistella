## Context

The enter-gate surgery collector (`tests/integration/surgery_collect.rs`) runs baseline → mutation → landing wait. On a successful landing it returns `LandingEvidence` (bounded poll history + baseline report), which callers format into an `EvidenceGuard` printed if a later failure unwinds past it. The successful mutation's own argv, exit status, and streams are returned to the caller for assertion and then dropped. The first honest-red capture showed why this hurts: the unit was gone by postmortem with no attributed actor, and the report could not name what ran immediately before the death.

## Goals / Non-Goals

Goals: retain the successful mutation's argv, exit status/disposition, and capped streams in `LandingEvidence`; render them in the guard output; pin the seam backend-free through the injected runner/clock. Non-goals: no new probes or backend queries; no verdict-semantics change; no post-mortem on the later path beyond reusing the retained set (the disclosed gap stays disclosed).

## Decisions

- **Retain a rendered mutation-identity section, not the raw `Output`.** `LandingEvidence` is already rendering-only (`history_report: String`, `baseline_report: String`), so an `Output` with unbounded `Vec<u8>` streams would be the one unbounded, non-rendered field in a struct whose purpose is boundedness — the cap has to be applied at the boundary anyway (the same `escape_capped` call), so storing the struct buys no diagnostic gain. The retained section is labelled rendered-and-not-re-runnable: argv joined as shell words (identity only, never re-execution) and streams escaped. Alternative (store the `Output`): rejected — unbounded streams and a wider struct for no diagnostic gain.
- **Render order: mutation first, then history, then baseline.** Rationale: the mutation is what a later failure most needs (what ran), history shows what followed, baseline shows what preceded. Callers extend their existing format string; no guard mechanics change.
- **Cap 4000 kept characters per stream, following `POSTMORTEM_STREAM_CAP`.** Rationale: the closest sibling is the postmortem's retained streams — streams a human reads after a failure — at 4000 kept characters via the same `escape_capped` helper whose counts are characters, not bytes (escaping expands, so kept characters ≠ rendered bytes). The general one-line cap (`RENDER_CAP = 160`) is too tight for failure-context streams. A failed mutation retains nothing, so the cap governs successful-mutation output only.

## Risks / Trade-offs

- [Risk] Retained streams could bloat failure output on verbose mutations → Mitigation: hard 4000-kept-characters cap per stream with explicit truncation marker; typical content is tens of bytes.
- [Risk] Joining argv as shell words loses exact quoting → Mitigation: identity use only (which binary with which args), never re-execution; documented as rendered, not re-runnable.
- [Risk] Scope creep into post-mortem-on-later-path → Mitigation: explicitly out of scope; the retained set is reused, not extended with new queries.

## Migration Plan

Harness-only change; no product code, no CI workflow change, no rollout. Land behind normal review; existing pins keep passing unchanged (additive field, additive report section).

## Open Questions

None — seam, caps, and render order are fixed above; scope is bounded by the retained set.
