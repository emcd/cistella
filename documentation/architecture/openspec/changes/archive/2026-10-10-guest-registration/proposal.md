## Why

0.2.0 proved the external-guest architecture (Podman isolator + Landlock extension over the
versioned stdio wire) but admits guests by hardcoded closed sets: isolator `podman`,
extension `landlock`, everything else refuses. No third-party isolator or extension can exist
until registration replaces closed admission with verification — the ceiling the 0.2.0 design
deferred deliberately (operator decision 2026-09-27, `cistella:todos/36`).

## What Changes

- Guest registry: one explicit seat-local file (`registry.toml` under the XDG
  config dir, `format-version = 1` required, no layering, no URL bootstrap) with
  separate `[[isolators]]` and `[[extensions]]` tables — two genera, never unified
  (a future `landlock` isolator registers beside the `landlock` extension as a
  companion pair, by design). Each entry maps an exact name to a source (fetch URL +
  verification hash, or absolute filesystem path) plus per-guest static args.
- Verification reuses the established precedent: digest-pinned, framework-admitted entries
  (the 3.1 digest registry), never bare names and never PATH search. The digest anchors
  on the bytes executed (same-FD or private immutable copy, re-verified every use);
  a fetch that fails digest refuses pre-create, loud. No signature-shaped fields exist in v1.
- Role binding is two-layer: the table determines the kind (`[[isolators]]` entries
  resolve only from `[isolator]`, `[[extensions]]` only from `[[extensions]]`), then the
  exact hello role string compares pre-planning; hooks stay optional for registered
  extensions.
- Closed admission stays the default: unregistered names still refuse with the typo
  protection intact. Registration is opt-in per guest, never a widening of the default.
- Shipped guests (`podman`, `landlock`) keep working unchanged through sibling-relative
  discovery; they become the first registered entries and the builders' reference
  implementation alongside a new minimal example guest under `tests/fixtures/`.
- Explicit non-goals (per the todo): the pure-Landlock isolator sibling and shared
  Landlock code across Landlock isolator / Podman-Landlock / Docker-Landlock get designed
  then, not now. The generic Landlock extension over the current wire protocol stays
  compatible in either direction.

## Capabilities

### New Capabilities

- `guest-registration`: registry schema, sources (fetch + hash, path), per-guest
  args/options, verification and refusal semantics, discovery extension beyond
  sibling-relative.

### Modified Capabilities

- `guest-hosting`: closed admission (`unknown names refuse`) becomes registered admission
  (registered names resolve per the registry; unregistered names still refuse as today).

## Impact

- `src/profile.rs` (registry file location/resolution only — NOT a profile section, per
  the explicit decision against profile-inline registrations), new registry module
  (file parsing + same-FD ownership anchor + role binding), `src/framework/discovery.rs`
  (registered-source resolution integrated with the sibling-relative shipped path);
  `documentation/usage/profiles.md` gains the registry reference;
  `tests/fixtures/` gains the minimal guest; conformance suite gains registry
  refusal/verification pins. No wire changes (`extension-protocol`,
  `isolator-contract` untouched).
