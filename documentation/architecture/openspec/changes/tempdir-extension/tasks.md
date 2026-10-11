## 1. Prepare carries session identity

- [ ] 1.1 Session id in the prepare request: extend the wire request shape with the opaque session id (additive field, major unchanged); pin an actual old guest binary completing prepare against the new request (decoder tolerance, not assumed).
- [ ] 1.2 Runner threads the id: run_extension_prepare/run_prepare pass the conduct session id into the request; slice paths use the fixed prefix plus validated session charset plus randomness with exclusive-create (no unsanitized concatenation). Driver lease marker per hosted prepare (O_EXCL create before first spawn, atomic updates with guest pid/pgid, removal on proven shutdown + convergence/quiescence); pin lease lifecycle fast.

## 2. Declared RW honored

- [ ] 2.1 Remove refuse_extension_rw_mounts: contributions merge through central validators with no blanket refusal; existing RO pins stay green; add RW-contribution merge pins (valid topology merges, collisions still refuse).
- [ ] 2.2 Landlock FULL grant on the admitted /tmp slice: compose the carveout for the contributed target; undeclared /tmp stays denied under hook with /tmp/scratch outlet; pin admit (hooked /tmp writes) / sibling-deny (outside-/tmp EACCES) / nested-scratch triple live.
- [ ] 2.3 Pair composition: landlock plus one other extension answer prepare in declaration order, concatenate raw plans, merge once centrally and atomically; two non-landlock extensions refuse; pin the composed conduct (tempdir RW/FULL plus retained sibling denial) through production conduct, never a hand-composed fixture.

## 3. Tempdir reference extension

- [ ] 3.1 Reference guest binary: example-grade fixture (never an installed sibling), hello with its registered role plus mounts and cleanup advertisement, mints owner-only uniquely-named slice per prepare, answers the slice triple plus cleanup declaration; registry entry shape documented for builders.
- [ ] 3.2 Teardown reap: ONLY declared cleanup paths removed after converged teardown; root admission (recorded dev+ino identity plus parent association at merge, non-symlink, seat-or-root owned — directory-to-directory swaps refuse like symlink swaps, pinnable without privileges) separate from FD-relative traversal (ordinary files unlink unfollowed, nested symlinks unlink as entries, never traversed); undeclared contributions survive; removal failure is litter; reap waits for convergence; pin populated-slice removal, symlink-to-outside target survival, root-swap refusal, leak-free conduct, and failed-teardown litter reporting.
- [ ] 3.3 Prepare lease plus gc owns orphans: driver lease (driver pid + guest pid/pgid, observation-only, never signals) per hosted prepare; gc skips on live driver, live guest group, unreadable/malformed lease, or missing guest identity, treats dead-driver/dead-group leases as stale, reaps session-attributable orphans otherwise (including prepare-failure orphans with no merged plan), enumerates only the attributable prefix pattern; pin driver-SIGKILL-while-guest-paused live (no reap until proven quiescence, then orphan cleanup), malformed-lease skip, stale progress, and independent sessions.
- [ ] 3.4 Nesting conformance: /tmp slice with /tmp/scratch beneath (depth-ordered mounts, subtree handling preserved); pin the nested topology fast + live.

## 4. Dogfood and docs

- [ ] 4.1 profiles.md tempdir reference: registry entry shape, profile shape, slice lifecycle, rotation/decommission notes.
- [ ] 4.2 Agentmux acceptance: per-session RW /tmp backed by a host slice in a real seat; their /tmp/scratch workarounds removed; live tier on host per precedent.
- [ ] 4.3 Validate: cargo fmt, cargo clippy --all-targets -- -D warnings, fast suite plus live tier on host; openspec validate --all --strict; sync and archive.
