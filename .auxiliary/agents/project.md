# Project Guidance

Project-owned knowledge the generated `AGENTS.md` entrypoint must not own.
Structured tracking stays in `nb`.

## Purpose

Cistella is a CLI driver that launches and supervises one OCI container per agent development session. It implements the devcontainers model for agent harnesses: declarative per-session profiles, allowlist-only mounts, runtime-agnostic container integration, and credential-absence push enforcement. Agentmux ([github.com/emcd/agentmux](https://github.com/emcd/agentmux)) is one consumer that invokes cistella as the session command via its coder profile mechanism; other orchestrators can use the same driver contract. The project aims to make agent containerization reproducible, auditable, and safe to run with broad permission scopes inside the container.

## Tech Stack

- Language: Rust (edition 2024).
- Container integration: rootless container runtime for V1; `--userns=keep-id` for sane bind-mounted worktree ownership; runtime labels plus a sidecar `gc` verb for orphan reaping; Quadlet systemd user units for V1 long-lived supervision.
- Transport: host terminal multiplexer hosts the session's TTY; the container is a long-lived service started detached; the harness launches inside it via a runtime exec call (never via attach to PID 1).
- Identity: per-seat SSH signing keys plus `allowed_signers` for signed review commits; no authentication credential for code-hosting pushes inside the container.
- Mount model: allowlist-only schema with explicit host-source, container-target, and mode triples; driver exports matching environment variables in the container.
- Change management: OpenSpec (OPSX) per `openspec/` and the project's `AGENTS.md`.

## Notes

Design decisions live in OpenSpec specifications under `openspec/` and in source-tree READMEs under `src/**/README.md`.
User-facing usage, profile setup, and the trust model live in `documentation/usage/` (start at the repo `README.md`); maintainer workflows in `documentation/development/`.

- Team org, role ownership, signoff policy, and merge workflow: `cistella:coordination/general/3`.