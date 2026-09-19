# Cistella

Cistella gives every agent development session its own supervised
execution environment: a declarative profile selects the image, the
allowlist mounts, and the harness argv, and the driver owns the session
lifetime from start through teardown. The profile and lifecycle
contract is shaped to allow other runtimes later.

Today that environment is an OCI container: rootless Podman for
execution, Quadlet systemd user units for long-lived supervision,
runtime labels plus a sidecar `gc` verb for orphan reaping. The harness
launches inside via a runtime exec call (never via attach to PID 1).

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
