## Why

Nested-under-RO triples are admitted by validation but start only when the intermediate chain pre-exists in the RO source (preexists rule, `cistella:artifacts/3`) — otherwise the session dies in a runc restart loop (`mkdirat ...: read-only file system`), minutes after the actual mistake. Fail fast at conduct time with a typed error naming both sides, before any unit or scratch exists. Advisor-directed (tier-2 verdict on `mountpoint-ownership`); explicitly not parse-time blanket rejection, which would kill the valid dogfood topology.

## What Changes

- New conduct-time preflight in `conduct_session`, after mount merge + canonicalization, before unit/scratch creation: for each triple target strictly beneath an RO ancestor target, translate the container-relative suffix onto the ancestor's canonical host source and require the full destination chain to exist with directory type; nearest already-ordered ancestor mount supplies the namespace for deeper nesting.
- Any missing link (or wrong type) is a typed error naming the RO ancestor and the missing translated host path; nothing is created, no unit or scratch is touched.
- Runc remains authoritative: a host tree change between check and create can still fail at start, and the existing fail-closed teardown covers that race (no new handling).
- Host-path symlink/canonical translation is pinned: the check runs on canonicalized host sources, so aliased paths resolve before comparison.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mounts`: the overlap/topology requirement gains the preflight availability rule for descendant-under-RO targets.
- `runtime`: the conduct lifecycle gains the preflight step in its normative sequence (before unit/scratch mutation).

## Impact

- `src/mount.rs` (pure translation + existence check over explicit host paths) + `conduct_session` wiring pre-install; no profile schema, transport, registry, or CLI changes.
- Tests: preexisting-nested succeeds with writable child + RO parent; missing intermediate refuses pre-mutation with the typed error; session-directory-under-RO dogfood passes; symlink/canonical translation pinned; induced race leaves no residue.
