## Why

Flat template names have hit their limit: `{{project-name}}` is driver-provided, but agentmux needs to pass per-invocation data (`bundle-name` for the TMUX socket path — the hardcoded bundle name in two seat profiles today), and profiles want host environment (`{{environment:HOME}}` makes `container-home` portable across users). One flat namespace cannot express provenance, so authors cannot tell what a span depends on. Fix with qualified namespaces now, pre-1.0, while breakage is cheap.

## What Changes

- **BREAKING** Template spans gain `context:name` qualification with `:` separator: `{{core:container-home}}`, `{{core:host-home}}`, `{{core:project-name}}` (reparented natives; no grandfathering, pre-1.0 sharp break), `{{supplement:name}}` (caller-provided via new `--supplement k=v` CLI, repeatable; e.g. agentmux passes `bundle-name=infrastructure`), `{{environment:NAME}}` (host process environment at resolution time).
- `--supplement k=v` (repeatable, conduct only): unknown `supplement:` names are typed errors (callers must supply every name the profile uses); values are opaque until substitution and validated per-sink after substitution through the existing gates (decided in design). Supplements are trusted, non-secret caller metadata: no credential-name denial applies (the caller already controls profile, mounts, labels, and harness argv, so a deny would be bypassable by renaming) and callers must not bridge ambient secrets into supplement values.
- `environment:` reads host env lazily (only names actually referenced) under a fail-closed hybrid: a name resolves only when on a compile-time fixed allowlist of non-credential ambient vars (initial list: `HOME` alone — the sole concrete consumer is the `container-home` showcase; extended solely by code-reviewed change), while credential-shaped names are hard-refused even if allowlisted (case-insensitive match: exact `SSH_AUTH_SOCK` plus `*_TOKEN`, `*_SECRET`, `*_KEY`, `*_PASSWORD`, `*_CREDENTIAL*` patterns). Ordering is fixed: hard-deny check, then allowlist, then `std::env` lookup. Allowlist-absent names and denylisted names are distinct typed errors naming only the variable, never its value — the credential-absence posture extends to templates.
- `container-home` gains an explicit early expansion phase (a fifth template sink alongside mounts, command argv, env values, and labels values): allowlisted `{{environment:*}}` and supplied `{{supplement:*}}` spans inside `container-home` resolve before the standard pipeline, then canonicalization and sensitive-root validation run on the substituted value, then `core:container-home` is constructed from it. Self-referential `{{core:*}}` spans inside `container-home` are typed cycle errors; no-rescan applies. This phase is what makes `container-home = '{{environment:HOME}}'` portable.
- All three namespaces expand in mounts, command argv, env values, and labels values — plus the `container-home` early phase above — under the same ordering, precedence, laziness, and no-rescan rules.

## Capabilities

### New Capabilities

- `supplements`: caller-provided template data via `--supplement k=v` and the `supplement:` namespace.

### Modified Capabilities

- `mounts`: template requirement moves to qualified namespaces (`core:`, `environment:`), with the `container-home = '{{environment:HOME}}'` portability showcase in examples.

## Impact

- `src/profile.rs` template engine (span parser gains namespaces, value tables per context, new CLI plumbing through conduct); seat profiles migrate to new spellings; agentmux coder entries pass `bundle-name` (companion change on their side).
- Unknown-span errors become namespace-aware (unknown context vs unknown name distinguished).
- Full unit matrix per namespace + live conduct with supplement/env coverage.
