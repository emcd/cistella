## Why

Seat homes are unwritable even where ownership and mode allow (cistella:issues/26): probes on a live seat show create denied on the tmpfs home root and on unmounted me-owned subdirs, while every bind-mounted subtree allows writes. Enforcement is grant-granular (Landlock "grant follows the rw triple"), not ownership — the earlier chown framing was investigated and withdrawn. This blocks agent flows that assume a writable home (`~/.local/bin`, site-packages, per-user state). The home tmpfs root is deliberately outside the mountpoint chown rule and carries no FULL grant today, so there is no path to a writable home without a confinement change.

## What Changes

- New optional profile field `home-source-class` (`container` | `host`) plus `home-host-source` path required exactly for `host`. Absent means container-private (the only topology constructible today: bare `tmpfs`).
- One authoritative home descriptor (canonical target + class + backing) constructed at validation, bound into the compile snapshot fingerprint, and shared by the supervisor FULL derivation, the RO-retention sets, and the mount renderer — so compiler, accounting, and renderer cannot disagree about the actual backing. The construction matrix is pinned: absent/container with no host source constructs tmpfs; host source with absent/container class refuses typed; host without path refuses typed; host with path refuses as unsupported construction before any mutation in this release.
- The Landlock compiler grants FULL on container-private home targets and folds the route into the existing FULL accounting, so RO triples nested under home keep their `:ro` VFS bindings via the current retention machinery (Landlock union cannot subtract; the binding carries RO). Host-class homes are declared, never constructed here.

## Capabilities

### New Capabilities

- `session-home`: session-home source declaration and writable-home grant semantics.

### Modified Capabilities

- `mounts`: profile schema gains `home-source-class` (+ host path when `host`); grant computation covers the home target.

## Impact

- `src/framework/landlock_compiler.rs` (home descriptor, snapshot binding, grant set + FULL accounting), `src/profile.rs` (schema, validation, defaults, construction matrix), `src/mount.rs` + both isolators (descriptor-driven home source interpretation), QA live proof on a podman seat (writable home, RO stays RO). No transport/wire changes.
