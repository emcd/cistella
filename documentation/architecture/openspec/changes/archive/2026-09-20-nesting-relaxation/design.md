## Context

`validate_mounts` (`src/mount.rs`) runs pairwise overlap checks over profile triples, CLI triples, and the worktree triple (pushed into the same list by conduct). The old `overlap_allowed` predicate permitted stacking only over read-only ancestors — a leftover of the shared-RO-`~/src` era, when any nesting was impossible and the RO-ancestor carve-out was the only valid shape. The nested-ro preflight (`nested_ro_checks`) is independently mode-aware (RO ancestors only), and `podman_volume_args` independently sorts shallower targets first.

## Goals / Non-Goals

**Goals:**
- Any strict ancestor/descendant stacking validates, in every mode combination.
- Exact duplicates still refuse (ambiguous intent, not stacking).
- Parent-first mount order pinned at unit level, not just live-verified.

**Non-Goals:**
- No change to preflight (RO-only), sensitive-root, home-shadow, or duplicate rules.
- No change to preparation (mode-agnostic mkdir walks confirmed).
- No seat-profile migration in this change (interim side-by-side posture stands until seats restart onto approved code).

## Decisions

- **Mode-agnostic predicate, signatures kept.** `overlap_allowed` / `overlap_allowed_host` keep their mode parameters (underscore-prefixed, documented as intentionally ignored) to preserve call-site symmetry; the predicate reduces to strict-nesting-or-refuse.
- **Ordering guarantee rests on the pre-existing sort.** `podman_volume_args` sorts by slash count ascending (stable); the new `volume_args_mount_parent_before_child` unit test pins child-first declarations emitting parent-first volumes.
- **Spec updated inline, recorded retroactively.** The live `specs/mounts/spec.md` normatively encoded the old rule, so the implementation commit carried the spec edit with reviewer visibility; this change re-records the same wording as a delta for the proposal track (tier-1 direction).

## Risks / Trade-offs

- [Risk] Shadowing semantics (child hides parent subtree) now reachable in RW combinations → Mitigation: intended and documented (deepest mount wins); the exact-duplicate refusal still catches typos.
- [Risk] Retroactive proposal weakens proposal-before-code discipline → Mitigation: one-off, reviewer-directed, 0-1-0 blocker urgency; content identical to the reviewed implementation.

## Migration Plan

None (relaxation only widens acceptance; previously valid profiles validate identically).

## Open Questions

None.
