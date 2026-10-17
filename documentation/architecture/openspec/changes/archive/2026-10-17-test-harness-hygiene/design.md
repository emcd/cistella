## Context

reviews/12 (Advisor, read-only source audit at `baa7b98`) plus QA's
pattern book and the owner inventory (710 tests at the time)
converge: no test deletion/demotion approved without
requirement-mapping + retained boundary + negative controls, and
harness defects must be fixed before counting retained coverage.
The no-wait-and-pray principle (project.md, 2026-10-16) makes this
change the foundation track: bounded observation first, handshake
conversions (change 2) and compressions (change 3) after.

## Goals / Non-Goals

- Goals: hard time bounds on harness calls; interference-free
  fixtures; ownership-disciplined shared namespaces; capability
  tags + logged skips; litter rule applied; diagnostics on all
  failure paths; four files split with margin.
- Non-Goals: no test deletions or demotions (change 3); no
  handshake conversions (change 2); no new product surfaces
  (todos/49 stays open); no behavior change in splits.

## Decisions

- Per-call timeouts over loop restructuring: keep existing poll
  shapes (reviewed, live-proven), add bounded primitives beneath
  them — smallest diff that makes deadlines truthful. Each poll
  composes with the REMAINING outer deadline, never a fresh full
  budget; a timeout bounds controller progress through wait,
  kill/reap, and pipe draining/EOF (no recv-then-unbounded-join
  recurrence). Nonblocking-setup errors and held-open descendant
  FDs surface as timeout/unproven cleanup, never success.
  Process-group/foreground/PTY semantics preserved; only fixture-
  owned groups are ever killed.
- Negative harness cases pinned, not just happy paths: silent
  id-line child, stdout/stderr flood or held writer, stuck poll
  command, timeout cleanup and failed cleanup. Bounded
  diagnostics must not silently truncate data used as correctness
  evidence — surface overflow or use a bounded-capture strategy
  that keeps assertions valid.
- Guard gc scoping by session ownership (guard only reaps its own
  id + lock-protected orphans), not by removing the safety net:
  cross-test reaping is the defect, cleanup itself is load-bearing.
  Lock-held does not mean owned: cleanup uses only minted IDs and
  owned fixture records, never globally destructive gc behind a
  lock. Unique names must survive concurrent suite INVOCATIONS,
  not just sibling collisions.
- Namespace rule (from the registry sess-N fix): filter to
  caller-owned infixes or serialize; fixed shared names across
  parallel tests are the smell, audited file by file.
- Capability tags and logged skips record applicability, not
  execution: executed vs soft-skipped bodies reported distinctly;
  final-hash full-live CI/pre-push gates stay intact regardless.
- Splits follow existing module seams (await mechanics set the
  precedent with `podman_await`/`corroboration`); each moved unit
  keeps its tests green at every step (commit per split), with
  move-only evidence kept separate from logic edits.

## Risks / Trade-offs

- [Tighter bounds expose slow-seat flakes] → Mitigation: bounds
  set from observed seat maxima + headroom, QA confirms on the
  live seat before merge; any new timeout is itself evidence,
  not noise.
- [Split churn touches reviewed code] → Mitigation: moves only,
  no logic edits in split commits; fast suite green per split.
- [Capability tags rot] → Mitigation: tags live beside the gate
  check that enforces them, reviewed together.

## Migration Plan

Implement track-internal order (bounds → interference →
ownership/tags → diagnostics → splits), Tier-1 → Tier-2, QA loop
focused on interference-sensitive tests, merge. Changes 2 and 3
stack after.

## Open Questions

- None blocking; bound values finalized against QA seat timings
  during implementation.
