## Context

Profile env today is a single `[environment]` table (`profile.environment: HashMap<String, String>` in `src/profile.rs`): values template-expand, `HOME` is refused there (derived from `container-home`), and the map renders into unit `Environment=` lines via `quote_systemd` (`src/runtime.rs`), with `HOME` additionally baked (`Environment=HOME=`) and forwarded at exec time (`src/transport.rs`). There is no path for invoker-owned process env to cross the conduct boundary — the 0.1.0 fleet incident. Stakeholders: operator (doctrine owner, explicit override recorded), Advisor (tier-2, tightened semantics adopted), Coordinator (discoverer).

## Goals / Non-Goals

**Goals:** exact-name required acceptances with fail-before-session/runtime-mutation semantics; collision errors covering assignments and driver-owned names; verbatim values through the shared safety gate; value-free diagnostics; zero new hardcoded deny checks on the acceptance path; pre-1.0 schema break (`[environment-assignments]`) with guided migration.

**Non-Goals:** globs/regexes; optional (non-required) acceptances; profile overrides of template allow/deny tables; credential-surface changes; `quote_systemd` redesign beyond the line-break gate needed here.

## Decisions

- **Top-level `environment-acceptances: Vec<String>`, default empty.** Alternatives: nested under `[environment]` (rejected — acceptances carry no values and skip template resolution; nesting would be a category error) or a conduct CLI flag like `--pass-env` (rejected — the profile must remain the complete audit artifact of container env; a flag splits intent across caller and profile). Bare-name list is also deliberate good sugar: the overwhelmingly common case (required, non-renderable) needs no attribute ceremony.
- **Union reserved, not built.** When attributes gain a consumer, `environment-acceptances` SHALL extend as a union — bare-name list or name→attributes table (untagged-enum deserialization), never a switch. List entries are defined as defaults-equivalent (`optional = false`, `renderable = false`); the list form stays valid forever with identical semantics. Dual-shape validation is payable then, only if consumed. This reservation is recorded here, in field rustdoc at implementation, and in the standalone 0.2.0 follow-up (outside the change lifecycle, so archiving cannot drop it) — no second proposal is owed for the shape itself, only review of the attribute semantics when they land.
- **Rename `[environment]` to `[environment-assignments]`.** Struct-field rename; `deny_unknown_fields` turns old profiles into a typed unknown-field error (pre-1.0 breakage accepted). Alternative of keeping the old name was rejected: the set-vs-accept distinction is the trust story of this change and the schema should state it.
- **Snapshot-then-apply.** Read all accepted names from the invoker env up front (profile-list order, deterministic first error); any absent name aborts before any session/runtime mutation — no unit, scratch, or container residue (XDG profile seeding during named lookup precedes parsing and is outside this boundary) — with a name-only diagnostic. `std::env::var` already rejects non-Unicode host values — that arm is free.
- **Collision = error, broadly.** Check acceptances against assignment keys AND driver-owned/implicit names. The implicit set is closed and pinned: `HOME` always, plus `SSH_AUTH_SOCK` if and only if the active credential-surface is `ssh_agent` (verified: `ssh_agent_volume_args` in `src/identity.rs` is the sole injector; `none` injects nothing). Future credential-surface variants that inject new names extend this set as part of the credential-surface change. Either-wins was rejected: silent shadowing in either direction is false auditability.
- **Verbatim means no template parse/rescan, not no validation.** Accepted values enter the env map post-substitution and render through the same `Environment=` path, but first pass a shared value gate that rejects line-breaking values value-free (the `quote_systemd` CR/LF weakness Advisor identified). Applying the gate on the shared rendering path hardens assignments as a side effect — accepted as in-scope, since acceptances cannot render safely otherwise. `=` inside values stays permitted.
- **No deny on acceptances, by explicit operator override** (over Advisor's 0.1.1 retain-deny counsel, recorded in the proposal). Rationale: the exact list is itself the intent record; accepted secrets are conveyed, never displayed (value-free diagnostics, no rescan into template error paths). Governing doctrine: exact-name declarations are deliberate operator intent — neither the deny lists nor future reviewers get a paternalist veto over what an operator does with a variable they named exactly (including forwarding or, later, rendering secrets). The only legitimate future questions about such attributes are semantics and mechanics, never permission.
- **No duplicates; env-name grammar validated** on acceptance names, deterministically (first duplicate in profile order errors).

## Risks / Trade-offs

- [Risk] Accepted secrets rest transiently in generated unit files for the session lifetime → Documented in trust-model notes as operator-intended conveyance on a trusted host; units are torn down with the session. Not mitigated technically in 0.1.1 by design.
- [Risk] Pre-1.0 schema break bites any out-of-tree profiles → Old `[environment]` tables fail fast with a typed error naming the rename; migration is two host profiles plus docs. Caveat-emptor pre-1.0 posture covers the rest.
- [Risk] `HOME`/implicit-name collisions surprise profile authors → The error message enumerates the implicit set; accepting `HOME` was never meaningful (driver-derived).

## Migration Plan

Implement → unit matrix + live conduct proving relay-var forwarding → migrate two host seat profiles + baked example + `documentation/usage/profiles.md` + trust-model note → review (tier-1; tier-2 optional, decision record already holds Advisor's counsel and the override) → 0.1.1 tag. Rollback is profile revert; no state migration involved.

## Open Questions

- Exact implicit-name enumeration for the collision set (implementer reads the credential-surface injection path; `SSH_AUTH_SOCK` conditionality must be visible at validation time).
- Whether the unknown-field error for legacy `[environment]` needs a custom rename hint or the default diagnostic suffices.
