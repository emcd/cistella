## Context

`validate_mounts` (pure, string-level) admits descendant-over-RO-ancestor triples per the reviewed stacking rule; the OCI runtime honors them only under the preexists rule (`cistella:artifacts/3`, five probes + mountinfo forensics). The gap between admission and start is a restart loop with a raw runc error. The driver runs on the host with full filesystem visibility at conduct time, so it can check availability before mutating anything. `conduct_session` (`src/main.rs`) already orders merge → validate → scratch → install → start; the preflight slots between validate and scratch.

## Goals / Non-Goals

**Goals:**
- Every nested-under-RO admission pays its availability check before the first mutation, with errors naming both sides.
- Zero behavior change for all other topologies (check is a no-op scan when no RO ancestor exists).

**Non-Goals:**
- No parse-time rejection (would kill the valid dogfood topology; Advisor explicitly ruled it out).
- No new races introduced: check-then-create is preflight only; runc authoritative, teardown covers.
- No profile schema or CLI changes.

## Decisions

- **Check in `conduct_session`, not in `validate_mounts`.** Validation is pure and string-level (unit-testable without a filesystem); existence lives on the host fs and belongs to the impure conduct path, next to the other pre-mutation gates. The pure part (suffix translation onto the ancestor source, nearest-ancestor selection) lives in `mount.rs` as unit-testable functions over explicit paths; the fs touch (`is_dir` per chain component) happens in the conduct wiring.
- **Translate, don't compare strings.** For descendant target D beneath RO ancestor A (both canonical container paths), strip A's prefix to get the relative suffix, join onto A's canonical host source, and require each chain component to exist as a directory. Nearest already-ordered ancestor wins for deeper nesting (sort by depth, first match supplies the namespace) — mirroring the mount application order so the check sees what runc will see.
- **Type-check directories, not just existence.** A file where a directory must be is the same failure one step later; check `is_dir` per component and name the offender.
- **Symlink/canonical translation pinned by using canonicalized inputs on both sides.** The merge pipeline already canonicalizes (`canonicalize_host_source` longest-prefix, container target cleaning); the preflight consumes those outputs, so an aliased host source resolves before suffix translation. A dedicated test pins a symlinked source resolving through.
- **Failure precedes all mutation.** The check runs before scratch creation, unit install, and start — a refusal leaves literally no residue (stronger than teardown-covered: nothing to tear down). Unit file and scratch absence is asserted in the refusal test.

## Risks / Trade-offs

- [Risk] Host tree changes between check and start (TOCTOU) → Mitigation: accepted preflight limitation, stated normatively; runc authoritative, existing teardown covers the residue. The window is milliseconds inside one conduct.
- [Risk] Symlink swaps on the host between check and start → Mitigation: same as above; host is trusted, sessions are owner-operated.
- [Risk] Over-strict type checks break exotic-but-working setups (e.g. file mountpoints? triples target dirs by construction) → Mitigation: targets are directories by schema; files as *intermediates* are genuinely broken (cannot mount beneath a file).
- [Risk] Performance: one `stat` per chain component → Mitigation: chains are short (depth-bounded by profile sanity); negligible next to container start.

## Migration Plan

None. New gate, no profile changes; previously-crash-looping conducts now fail fast with a typed error, everything else byte-identical behavior.

## Open Questions

None. Advisor specified the mechanism and tests; this records the placement.
