# Design: tempdir extension

## Context

0.3 proved external guests resolve, verify, and host (registry + same-FD anchor + role binding), but every extension in the tree is either shipped (Landlock) or a test peer. Session `/tmp` today is the image's own, with the driver-appended `/tmp/scratch` outlet as the sole writable path under confinement. Agentmux burns rounds on that limitation. Stakeholders: seat operators (registry authors, profile authors), Agentmux (first consumer), reviewers (tier-1 + tier-2 — the RW admission reverses a tier-2 hardening, carried here by explicit operator direction).

## Goals / Non-Goals

Goals: a third-party extension providing per-session RW `/tmp`, resolved through the seat registry; prepare carrying session identity; declared-RW honored end to end (merge, Landlock, launch); contributed host state reaped at teardown. Non-goals: removing `/tmp/scratch` (later migration); the full handler-registry redesign (`cistella:todos/44` — this change adds only the reap hook point the teardown needs); multi-extension composition (still v1 single); Windows/macOS seat paths (Linux-only, as today).

## Decisions

- **Session id rides the prepare request as an opaque string.** The driver already mints it (container names, scratch paths); extensions receive it but never parse it (opaque session identifier, as the operator directed). Rejected: deterministic path conventions (fragile cross-component coupling), ambient env passing (sloppy authority), profile supplements (static per-profile values cannot carry per-session identity). Additive field, major unchanged; old guests ignore unknown request fields.
- **RW contributions merge, then enforce centrally.** `refuse_extension_rw_mounts` is deleted, not narrowed: operator direction is honor-whatever-is-contributed, no exceptions. Safety stays in the central validators that already own each set (mount topology validation, collision refusal, lattice evaluation, hook-executable binding) — the same gates that vet RO contributions today. Rejected: scoped admission (e.g. RW-only-under-/tmp) — a second hardcoded policy would re-create the exact fight this change settles; declarations are authoritative intent.
- **The extension mints uniquely-named slices, the driver reaps by plan.** The guest creates `/tmp/cistella-tmp-<session>-<rand>` (session id for debuggability, randomness for uniqueness across re-prepares) with 0700 and contributes `{host, /tmp, rw}`. Teardown walks the merged plan's extension-contributed host sources and removes them after converged teardown (best-effort removal is litter, never residue — the unit is gone either way). Rejected: driver minting (keeps session-filesystem knowledge in the driver, against the operator's architecture), extension reaping (the guest is long gone at teardown; no live agent to call).
- **Landlock grants the admitted slice FULL, nothing else changes.** The compose rule already grants every declared RW target full rights wherever it sits; the `/tmp`-denied default stays for undeclared paths. The tempdir triple is the first RW triple targeting a system path, so it exercises the rule rather than adding one. `/tmp/scratch` nesting is preserved by depth-ordered mounting (`/tmp` before `/tmp/scratch`); the scratch triple keeps its existing subtree handling.
- **Vendoring: separate binary, registry-resolved, never a shipped sibling.** The reference extension builds as its own artifact (own `[[bin]]`, own release packaging) and resolves by registry entry — if it shipped beside the driver it would prove nothing about third-party hosting. Dogfood profiles name it; fleet installs it like any third-party guest. Its hello role is its own name; it advertises `mounts` (gate_capabilities admits nonempty mounts only when advertised).
- **Teardown hook point, not a handler registry.** The reap duty needs exactly one new lifecycle slot (post-converge host-state cleanup with the merged plan in scope). The general handler registry (`cistella:todos/44`) is designed separately; this slot must fit wherever it lands (flagged as an open question, as registration flagged the seat tier).

## Risks / Trade-offs

- [Risk] Malicious/buggy extension contributes RW `/` or sensitive targets → Mitigation: contributions were never trusted — topology validation, collision refusal, and Landlock scoping still vet every triple; profiles opt in per extension; registry is seat-operator-authored. The removed refusal was defense in depth, and the operator explicitly accepts its removal.
- [Risk] Slice leaks on teardown failure → Mitigation: best-effort removal plus `gc` coverage (same posture as scratch today); slice names carry the session id for attribution.
- [Risk] Two sessions share a slice (name collision) → Mitigation: randomness per invocation plus O_EXCL-style create-fail retry in the guest; collision fails the prepare loudly, never shares.
- [Risk] `/tmp/scratch` outlet interplay (nested triple under the new `/tmp`) → Mitigation: depth-ordered mounts plus existing subtree handling; pinned by a nesting conformance test.

## Migration Plan

Additive: new extension binary (absent unless installed), new registry entries (absent unless authored), new prepare field (ignored by old guests), deleted refusal only affects profiles declaring extensions that contribute RW (none exist today — Landlock contributes RO + hooks). Rollback is commit revert plus registry-entry removal; with no tempdir entries every profile behaves exactly as before. `/tmp/scratch` untouched.

## Open Questions

- Handler-registry fit (`cistella:todos/44`): the reap slot defined here must fit wherever that registry lands — flag at review if it conflicts.
- Should the reference extension live in-tree as a `[[bin]]` or move to its own crate once the pattern stabilizes? Default to in-tree `[[bin]]` for v1 (reviewable beside the framework), revisit on the second third-party guest.
