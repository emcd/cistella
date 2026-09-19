# supplements Specification

## Purpose
Caller-provided template data for profiles, keyed by name and supplied per conduct invocation.

## Requirements
### Requirement: Supplement data for templates
`conduct` SHALL accept repeatable `--supplement <key>=<value>` pairs (conduct only; last-wins on duplicate keys; conservative ASCII key charset). Profiles referencing `{{supplement:<name>}}` resolve each name from the supplied pairs; any unsupplied referenced name is a typed error. Supplement values are opaque until substitution: no generic charset gate applies to the raw value (in particular no project-name gate — a supplement may supply a whole path such as `--supplement home=/home/me` for `container-home`); each fully substituted sink applies its existing validator (`container-home` absolute/canonical/sensitive-root, mounts topology/injection, labels/env rules, argv controls). Supplements expand everywhere templates expand (mount triples both sides, command argv, env values, labels values, and the `container-home` early phase) with no new expansion semantics. Supplements are trusted, non-secret caller metadata: no credential-name denial applies to supplement values, and callers SHALL NOT bridge ambient secrets into them.

#### Scenario: Supplied supplement resolves
- **WHEN** conduct runs with `--supplement bundle-name=infrastructure` against a profile using `{{supplement:bundle-name}}`
- **THEN** the span resolves to `infrastructure`

#### Scenario: Unsupplied supplement fails closed
- **WHEN** a profile references a supplement name not supplied on the invocation
- **THEN** conduct fails with a typed error naming the missing key before any mutation
