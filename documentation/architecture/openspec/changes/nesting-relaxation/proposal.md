## Why

The overlap rule stacked mounts only over read-only ancestors, so a wholesale `~/src` RW profile triple collided with pair-form worktrees nesting beneath it (`overlapping mounts` refusal). That refusal took down both dogfood seats on restart. Side-by-side per-project triples do not scale; strict nesting under RW ancestors is mechanically sound (mkdir works, podman mounts parent-first). Relax the rule now, pre-1.0, as a 0-1-0 blocker. (Retroactive record: implemented and tier-1-approved as `7f34fd8`; this change exists so the contract relaxation is on the proposal track, not hidden in an implementation commit.)

## What Changes

- **Contract relaxation** (normative): strict ancestor/descendant triples always stack regardless of modes; only exact canonical-target duplicates refuse. Podman receives parent-first ordering; `mkdir` works through RW parents; nesting under RO ancestors stays covered by the nested-ro preflight (preexists rule).
- `overlap_allowed` and `overlap_allowed_host` become mode-agnostic (modes ride along unused, signatures kept symmetric). Worktree-vs-triple checks inherit the relaxation through the shared predicate.
- Unit matrix rewritten to the new contract plus a parent-first ordering test and a host-mirror test; live `nested_rw_under_rw_conducts` proves the seat-restart shape end to end.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mounts`: stacking rule relaxed from RO-ancestor-only to any strict nesting; scenarios updated (RW-combination stacks, duplicates still refused, canonicalize reframed).

## Impact

- `src/mount.rs` predicates only; `prepare.rs` already mode-agnostic (confirmed by reviewer read); `podman_volume_args` ordering unchanged (pre-existing parent-first sort carries the guarantee).
- Seat profiles may use wholesale ancestors again (follow-up, not this change).
