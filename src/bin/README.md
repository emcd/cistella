# Shipped guest binaries

Executables that run **inside** the container as framework guests. Each speaks
the versioned stdio wire protocol (`src/framework/protocol*.rs`) back to the
driver-side `GuestHost`, which spawned it with pinned FDs plus a hello and
capability exchange. Guests never touch the host directly; everything they
need arrives via prepared mounts, environment assignments, and staged hook
artifacts.

## Binaries

- `cistella-isolator-podman` — the isolator guest. Owns the stdio loop;
  op semantics live in `IsolatorGuest` (`src/isolators/wire.rs`) so host
  and guest share one dispatch. See `src/isolators/README.md` for the
  lifecycle this guest drives.
- `cistella-extension-landlock` — confinement extension. Compiles the
  Landlock policy (`framework/landlock_compiler.rs`) into a staged wrapper;
  the only shipped extension that contributes hooks.
- `cistella-extension-ssh` — SSH extension. Manages `~/.ssh` inside the
  container (mount plus permissions) so agent forwarding and known-hosts
  work without leaking host keys.
- `cistella-extension-tmpdir` — tmpdir extension. Provides a writable
  container-private `/tmp` (tmpfs), replacing the earlier forced-`TMPDIR`
  workaround. This is why container `/tmp` is writable while other
  host-mounted paths may be read-only.
- `cistella-supervisor-landlock` — Landlock wrapper binary: the
  in-container exec ancestor, applying confinement around spawned
  commands.

## Conventions

- Guest `main()` functions are thin: parse the rendezvous FDs from argv,
  run the hello exchange, then dispatch ops in a loop. Shared logic lives
  in `src/framework/`, never duplicated per guest.
- A guest that needs host resources declares them through its extension
  (`PreparePlan`: env, mounts, claims, hook requests) — it does not open
  them itself. The framework vets, stages, and mounts; see
  `src/framework/` for the prepare/merge/stage/launch pipeline.
- Registry entries pin each guest binary by digest; the driver resolves
  the verified binary, role, and args from the seat-local registry.
