# Source layout

The driver (`src/main.rs`, `src/cli.rs`) conducts sessions; the
framework (`src/framework/`) hosts external guests over a versioned
stdio wire; `src/isolators/` owns the isolator lifecycle;
`src/bin/` holds the shipped guests.

## Guest hosting architecture

```
driver (conduct, src/main.rs)
 ├─ profile ── declares ──▶ [isolator] + [[extensions]]   (names only)
 ├─ registry ── resolves ─▶ verified binary + role + args (seat-local)
 ├─ GuestHost ── spawns ──▶ pinned-FD exec + hello + capabilities
 ├─ prepare ── asks each extension ─▶ PreparePlan { env, mounts, claims, hooks }
 │     │                                 ▲ extension-PROPOSED hook requests
 │     ▼                                 │ (argv_prefix + digest-pinned artifact)
 ├─ merge (central, framework-owned baseline; collisions/claims/lattice)
 ├─ stage ── private verified copy, RO mount at /run/cistella/hooks/…
 └─ launch ── composes wrapper argv, isolator creates/starts
```

Hooks today are extension-proposed confinement wrappers that the
framework vets (the executable must be exactly the staged wrapper),
stages, and executes — the extension says "confine me thus," the
framework decides admissibility and performs staging. Only shipped
Landlock contributes hooks, and only it is required to (zero hooks
from a registered extension converges plain). See `src/isolators/`
for the lifecycle half and `documentation/usage/profiles.md` for
the registry reference.
