# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-20

### Added

- One container per agent session: `conduct` starts a session from a
  profile and tears it down afterwards; `enter`, `survey`, `inspect`,
  `terminate`, and `gc` manage running sessions and reap orphans.
- Declarative TOML profiles (image, allowlist-only mounts, harness
  command, environment, labels) with tiered lookup and seeded starter
  examples.
- Reusable profiles through qualified template spans: `core:`
  (driver values like project name and home directories),
  `supplement:` (per-invocation caller data via `--supplement k=v`),
  and `environment:` (host environment, currently `HOME`).
- Credential-absence posture: no code-hosting credentials inside seats;
  an opt-in sign-only agent socket covers commit signing, and
  credential-shaped host variables can never leak through templates.
- Prompt container stop, prepared mount ownership, and fail-fast errors
  for mounts nested under read-only parents.
- User documentation (usage, profile reference, trust model and
  limitations) and a maintainer guide.

