# mounts Specification Delta

## MODIFIED Requirements

### Requirement: Mount validation (canonicalize, reject unsafe/colliding, two-tier topology)

The driver SHALL canonicalize both sides of every mount triple and `container-home` before validation and SHALL reject any profile where `container-home` or a container-target is at or above sensitive roots, where two triples share an exact canonical target, or where a triple shadows session-home. Strict ancestor/descendant triples always stack regardless of modes (deepest mount wins, mounted in depth order: parents before children). Nesting under read-only ancestors additionally requires the nested-ro preflight (preexisting host chain, typed refusal otherwise); nesting under read-write ancestors proceeds by `mkdir` through the parent.

#### Scenario: Read-write nesting stacks in any mode combination
- **WHEN** triples nest in RW-under-RW, RO-under-RW, or RO-under-RO combinations (e.g. a wholesale `~/src` RW ancestor with a pair-form worktree beneath it)
- **THEN** validation succeeds and podman mounts parents before children; only exact duplicate targets refuse

#### Scenario: Exact duplicate targets still rejected
- **WHEN** two triples share a canonical target, or either triple shadows session-home
- **THEN** validation fails with `duplicate mount target` or the session-home error before any container is created
