## 1. Prepare carries session identity

- [ ] 1.1 Session id in the prepare request: extend the wire request shape with the opaque session id (additive field, major unchanged); old guests ignore it; pin request-shape and ignore-unknown-fields pins.
- [ ] 1.2 Runner threads the id: run_extension_prepare/run_prepare pass the conduct session id into the request; pin the id reaching the guest (peer echoes it in a diagnostic-safe way).

## 2. Declared RW honored

- [ ] 2.1 Remove refuse_extension_rw_mounts: contributions merge through central validators with no blanket refusal; existing RO pins stay green; add RW-contribution merge pins (valid topology merges, collisions still refuse).
- [ ] 2.2 Landlock FULL grant on the admitted /tmp slice: compose the carveout for the contributed target; undeclared /tmp stays denied with /tmp/scratch outlet; pin admit/deny/outlet triple live.

## 3. Tempdir reference extension

- [ ] 3.1 Reference guest binary: own artifact (own [[bin]]/example target to start), hello with its registered role plus mounts advertisement, mints owner-only uniquely-named slice per prepare, answers the slice triple; registry entry shape documented for builders.
- [ ] 3.2 Teardown reap: contributed host directories removed after converged teardown (litter, never residue); reap hook slot where the lifecycle needs it; pin leak-free conduct plus failed-teardown litter reporting.
- [ ] 3.3 Nesting conformance: /tmp slice with /tmp/scratch beneath (depth-ordered mounts, subtree handling preserved); pin the nested topology fast + live.

## 4. Dogfood and docs

- [ ] 4.1 profiles.md tempdir reference: registry entry shape, profile shape, slice lifecycle, rotation/decommission notes.
- [ ] 4.2 Agentmux acceptance: per-session RW /tmp backed by a host slice in a real seat; their /tmp/scratch workarounds removed; live tier on host per precedent.
- [ ] 4.3 Validate: cargo fmt, cargo clippy --all-targets -- -D warnings, fast suite plus live tier on host; openspec validate --all --strict; sync and archive.
