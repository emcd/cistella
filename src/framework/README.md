# Framework: guest hosting over a versioned stdio wire

Hosts external guests (isolator, extensions) in the driver: spawn with
pinned FDs, hello and capability exchange, then framed request/reply ops.
The framework owns the trust boundary — guests propose, the framework
disposes.

## Pipeline (prepare → merge → stage → launch)

Defined in `prepare.rs`, orchestrated from `conduct.rs`:

1. **Prepare** — each extension returns a `PreparePlan { env, mounts, claims, hooks }`
   proposing what it needs (`prepare_select.rs` chooses the active set).
2. **Merge** — central, framework-owned baseline. Collisions, duplicate
   claims, and the permission lattice resolve here, not in extensions.
3. **Stage** — private verified copy of hook wrappers and artifacts,
   mounted read-only (e.g. `/run/cistella/hooks/…`). Only the exact
   staged executable is admissible.
4. **Launch** — wrapper argv is composed and the session starts.

Extensions say "confine me thus"; the framework decides admissibility.
Zero hooks from a registered extension converges plain — only Landlock
contributes hooks today, and only it is required to.

## Components

- `guest.rs` (`GuestHost`) — spawn, hello, fd rendezvous, op exchange.
- `protocol*.rs` — versioned framing and the op vocabulary shared with
  guests (`src/bin/`), so both sides cannot drift.
- `fdpass.rs` — file-descriptor passing plus diagnostics bundles.
- `registry.rs` — seat-local binary resolution: verified binary, role,
  and args per guest name. Refuses group/world-writable guest binaries.
- `credentials.rs`, `policy.rs`, `permissions.rs`, `signals.rs`,
  `stream.rs` — identity, admission policy, permission lattice, signal
  forwarding, and stdio streaming respectively.
- `contract.rs`, `expectations.rs`, `lifecycle.rs`, `isolator.rs` —
  the `Isolator` trait and session lifecycle states the driver walks.
- `discovery.rs`, `landlock_compiler.rs` — guest discovery and
  Landlock policy compilation for the confinement extension.

## Key invariant

Handles are framework-minted opaque strings end to end; guests never
invent them. Mounts, environment, and hooks in the launched session
derive only from the merged plan — nothing a guest asserts at runtime
bypasses prepare/merge/stage.
