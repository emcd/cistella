# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-09-29

### Added

- Framework-owned lifecycle (`src/framework/`): lifecycle/capability
  contract, isolator trait with Podman extracted behind it, versioned
  length-prefixed stdio protocol host with deterministic fake peer,
  single prepare transaction (typed env/mount sets, central merge,
  policy claims, guest-hook requests), severity × scope policy lattice
  with user `policies.toml` (exact-name acknowledgements, tighten-only,
  value-free diagnostics), and a credential seam of strict handle
  variants (`opaque-reference`, `seat-socket`) that never carries values.
- `conduct` evaluates profile assignments plus acceptances through the
  lattice before unit/scratch creation (refusal leaves no residue).
- Conformance harness: Podman lifecycle fidelity, protocol/prepare
  fault peers, Agentmux + SSH design-vector fixtures, Landlock spike
  (admitted/`EACCES`/typed `Unsupported`) plus keep-id namespace probe.
- External guest binaries: the Podman isolator and the Landlock
  extension run as separate processes speaking the versioned stdio
  protocol, discovered sibling-relative to the driver (never PATH).
  Production `conduct` drives lifecycle over the wire; the in-process
  implementation stays as the conformance reference, and any
  behavioral divergence surfaces as a conformance failure. Session
  stdio crosses by explicit descriptor passing (PTY and piped modes
  preserved); a missing or crashed guest fails loudly, never falls
  back silently.
- Landlock confinement for declared profiles (`[isolator]` plus
  `[[extensions]]`; profiles without them run unchanged): a staged
  wrapper applies a Landlock ruleset (kernel ABI ≥ 3) derived from
  declared mount modes before the harness execs, so broadly-mounted
  trees like `~/src` stay read-only where declared. Mount-alias
  grafts refuse by dentry identity, the syscall denial matrix carries
  errno evidence, and every shortfall (probe, attestation, transition)
  refuses typed pre-execute.

### Changed

- **BREAKING** (pre-1.0): the legacy unconditional token-assignments
  veto is removed; token-shaped assignment names refuse as
  `suppressible × universal` unless exactly acknowledged in user
  policy. Refusal error class migrates `Identity` → `Contract`
  (scripts matching on the old class need updating). Shipped
  `environment-acceptances` stay grandfathered against compiled
  defaults only; user rules take precedence. Absence-by-default is
  preserved: unacknowledged tokens still refuse.
- The hook diagnostics gate passes on the wrapper's transition proof,
  not on session EOF: long-lived harnesses are no longer capped at
  the pre-execute deadline.

## [0.1.1] - 2026-09-22

### Added

- Top-level `environment-acceptances`: exact invoker-environment names
  forwarded verbatim into container env (required, fail-before-mutation,
  collision-checked, value-free diagnostics, no deny). Unblocks
  containerized agentmux seats (relay `AGENTMUX_BUNDLE`/`AGENTMUX_SESSION`
  discovery).

### Changed

- **BREAKING** (pre-1.0): `[environment]` table renamed to
  `[environment-assignments]`, symmetric with acceptances. Legacy tables
  fail closed at parse time.
- Credential absence is now absence-by-default: explicit acceptances are
  an operator override (diagnostics display names only; values rest in
  unit `Environment=` lines for the session lifetime).
- `[environment-assignments]` values reject line breaks; accepted and
  assigned values share one safety gate (`=` permitted).

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
- Strict mount nesting in any mode combination (deepest mount wins),
  so pair-form worktrees may sit beneath profile ancestors.
- User documentation (usage, profile reference, trust model and
  limitations) and a maintainer guide.

