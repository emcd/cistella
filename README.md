# Cistella

Cistella isolates agent sessions via various technologies. 

Currently, Podman + Quadlet is supported. More to come.

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
