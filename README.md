# Cistella

Cistella isolates agent sessions via various technologies.

The framework owns session lifecycle (plan → gate → apply → create →
probe → confine → execute → await → teardown) and drives backends
through external guest binaries over a versioned stdio protocol.
Currently, Podman + Quadlet is supported. More to come.

## Confinement

Profiles declare backends and confinement explicitly:

```toml
[isolator]
name = 'podman'          # default; only podman today

[[extensions]]
name = 'landlock'        # default none = pure Podman behavior
```

Profiles without these keys run unchanged (0.1.x behavior). Declaring
the `landlock` extension engages the hooked conduct path: a staged
wrapper applies a Landlock ruleset inside the container before the
harness execs, derived from the profile's declared mount modes — so
broadly-mounted trees like `~/src` stay read-only where declared,
with typed refusals (not silent passes) on any confinement shortfall.
Unknown isolator/extension names refuse at conduct (closed admission).

## Documentation

- Profile setup: [documentation/usage/profiles.md](documentation/usage/profiles.md) —
  format, resolution tiers, template namespaces, credential surface
- Trust model and limitations:
  [documentation/usage/trust-model.md](documentation/usage/trust-model.md) —
  ambient authority, known caveats, planned future work
- Maintainer guide:
  [documentation/development/README.md](documentation/development/README.md) —
  images, validation tiers, hooks

## Requirements

- Rootless Podman 4.x and a systemd user manager on the host.
- A session image (see `data/dockerfiles/`; build and verify with
  `./data/dockerfiles/validate.sh`).
- For profiles declaring the `landlock` extension: a kernel with
  Landlock ABI ≥ 3 (Linux 6.2+), and crun as the Podman OCI runtime
  (runc is admitted only on podmans lacking `exec --preserve-fd`,
  via the fallback fd-set path — same guarantee).

## Install

```sh
cargo install --path .
```

Run `cistella check` for host preflight before the first session.

## Quick start

```sh
# Host preflight (exits zero only when dependencies pass).
cistella check

# One session: profile `opencode`, worktree defaults to the cwd.
cistella conduct --profile opencode -- opencode --auto

# Companion shell, list, post-mortem, teardown.
cistella enter <id-prefix>
cistella survey
cistella inspect <id-prefix>
cistella terminate <id-prefix>
cistella gc
```
