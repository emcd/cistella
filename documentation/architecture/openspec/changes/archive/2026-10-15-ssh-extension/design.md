## Context

v1 of this change kept `credential-surface` as intent and moved only mechanism; operator rejected the competing mechanism (2026-10-14): the slot is deleted and ssh stands alone as a declared extension. `ExtensionConfig` is name-only by doctrine (argv lives in the registry, never the profile), so the socket path cannot come from the declaration — it comes from seat ambient, read by the driver at selection like `HOME`/`TERM` snapshots. The fake guest's `prepare-vector-ssh` modes already pin the merge-side contract. Stakeholders: Agentmux fleet profiles (migrate out-of-band), QA (live throwaway-agent proof), RG/Advisor (full re-review — selection is load-bearing in every v1 finding round).

## Goals / Non-Goals

Goals: standalone ssh extension; total `credential-surface` deletion; first production triple pins. Non-goals: new forwarding capability; per-socket pinning in config; agent lifecycle; advisory policy warnings (no such lattice mode exists — separate proposal if wanted).

## Decisions

- **Delete the slot, migrate profiles, break sharply.** Alternative (v1): keep intent, move mechanism. Rejected by operator: two selection paths for one capability is confusing competing machinery. Pre-1.0 break-freely covers the migration; fleet updates ride out-of-band.
- **Ambient-sourced path, declaration-gated forwarding.** The driver reads invoker `SSH_AUTH_SOCK` at selection and passes `--socket=P --socket-canonical=C` (P ambient value, C fs-canonicalized). Alternatives: a socket field on `[[extensions]]` (violates participation-only doctrine, ssh-specific config surface); registry-baked argv (wrong layer — registry carries guest sources, and shipped-first would shadow it); guest reads ambient itself (config authority leaks to the guest). Ambient alone never injects — without a declared `ssh`, no agent moves, exactly as `None` behaved.
- **Declared ssh with unset/non-socket ambient refuses.** The profile promised forwarding the seat cannot provide — the silent-plain class. Fail-closed, typed, pre-create.
- **Standard shipped-extension path, no special-casing.** `ExtensionResolution::Ssh` + `ExtensionSpawn::Ssh`, sibling install-dir trust, declaration order, central merge. The only ssh-shaped code in the driver is reading ambient + building argv — the same shape as tmpdir's `--role=` precedent. No ssh-last rule, no auto-spawn, no reservation games beyond the declared-instance slot ownership below.
- **Declaration-bound slot ownership with expected tuple (carried from v1).** When `ssh` is declared, the name `SSH_AUTH_SOCK` is owned by the `shipped:ssh` instance by declaration: the merge exempts exactly that instance's contribution from the `DRIVER_ENV`-derived reservation and refuses every other claimant. The instance MUST return exactly `{env: (SSH_AUTH_SOCK, P), mount: (C → P, RO), permission: (P, read), handle: (C)}` — omission, divergence, extras, or empty env refuse whole. Undeclared: the reservation holds for all, as today. Permission names P (guest-visible map); mount source and locator name C (host authority).
- **Namespaced instance IDs (carried from v1).** `shipped:<selection>` / `registered:<entry>`; `:` refused in registry entry names at parse; registry entries named `ssh` refuse at parse (landlock shadowing pattern). A legal entry named `cistella-extension-ssh` resolves to `registered:cistella-extension-ssh`, disjoint from `shipped:ssh`. Landlock/tmpdir diagnostic owner strings change form (named pre-1.0 impact; no test pins them).
- **Generic instance-bound locator equality (carried from v1).** Admission parameter of declared `{instance_id, path}` locators; `SEAT_RUNTIME_DIR` prefix gate untouched for others. Guest kind gate: `S_ISSOCK` on C, no `read()` probing; `RO` is mount hygiene.
- **Policy evaluates uniformly (operator-ruled 2026-10-14).** The ssh contribution evaluates as `Extension` provenance; `OnExtensions` rules apply with no exemption. Policy wins over guests — a rule denying `SSH_AUTH_SOCK` at `on-extensions` refuses a declared-ssh profile loudly, and that is the engine reporting a policy/profile contradiction, not a defect. Acceptance-override doctrine stands: `environment-acceptances` naming `SSH_AUTH_SOCK` without declared ssh still forwards ambient verbatim (operator named it deliberately); with declared ssh it refuses (owned name).

## Risks / Trade-offs

- [Risk] Every agent profile breaks at parse until migrated → Mitigation: sharp break is operator-ordered; baked + test profiles migrate in-change; fleet out-of-band before 0.3 ships.
- [Risk] Profiles can no longer pin WHICH socket (ambient decides) → Mitigation: accepted consequence — agent forwarding is inherently seat-ambient; multi-socket seats resolve by invoker environment, same as `HOME`/`TERM`.
- [Risk] Live proof needs a real agent on the QA seat → Mitigation: throwaway `ssh-agent` socket, never the seat's own; `ssh-add -l` through the forwarded socket plus teardown-clean, missing/wrong-type/unset-ambient refusals, ambient-without-declaration controls.
- [Risk] Owner-string reformatting churns diagnostics/pins → Mitigation: no test pins shipped owner strings (verified synthetic-only); leases ephemeral.

## Migration Plan

Single change: guest binary + selection + parity pins (literal final argv for migrated agent/none profiles across 0/1/N extensions — non-symlink byte-identical, symlink `C:P` + declared-order positions excepted as documented), then slot + helper + join-block deletion with profile migrations in the same change, then QA live tier. Rollback is revert. 0.3 ships only after fleet profiles migrate.

## Open Questions

- None. Scope closed by operator directive (standalone ssh, slot deleted, 0.3).
