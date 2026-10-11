## Why

Sessions need a writable `/tmp` backed by host storage: Agentmux seats burn rounds working around the `/tmp/scratch` outlet limitation today, and hardcoded-/tmp writers (bun shims, build tooling) fail under confinement. The 0.3 registry proved third-party guests resolve and verify — now the architecture must prove a third-party guest can provide a real session service end to end.

## What Changes

- New separately-vended extension guest providing per-session `/tmp`: it mints a host slice per session, contributes it as an RW triple targeting `/tmp` plus an explicit cleanup declaration naming the minted directory, and the framework reaps declared directories at teardown. Registered through the seat registry like any third-party guest (first real dogfood of guest-registration).
- Prepare carries the opaque session id (the same id minted for containers), so extensions can mint per-session state. Additive wire field, protocol major unchanged; old request decoders must tolerate unknown fields (pinned against an actual old guest, not assumed).
- Extension mount contributions are honored as declared — including RW. The blanket `refuse_extension_rw_mounts` refusal is removed (operator direction: declarations are authoritative intent; Landlock enforces them, it does not second-guess them). No exceptions. Registry/profile opt-in is documented as operator trust delegation, not validator replacement.
- Landlock composition grants declared RW targets as FULL carveouts wherever they sit, including the admitted `/tmp` slice (carveout to the `/tmp`-denied rule, which stays the default for undeclared paths).
- Teardown reaps ONLY explicitly declared cleanup directories (never every contributed mount — a contributed RO project dir must survive), nofollow, after converged teardown; `gc` owns retryable cleanup of attributable orphaned slices (durable record: session id embedded in slice names). Lifecycle hook points land where the teardown needs them (first step toward the handler-registry reassessment, `cistella:todos/44` — not the full reassessment).
- Composition: v1 conducts landlock plus at most one other extension (the pair concatenates prepare plans and merges once centrally — declaration order, existing validators, atomic). General N-extension composition stays deferred; the tempdir proof (RW/FULL plus retained sibling denial) runs through production conduct on the pair.
- Explicit non-goals: removing the driver-owned `/tmp/scratch` outlet (direction is isolator-plus-extensions owning session filesystem services, but scratch removal is a later migration with fleet impact — not this change); the full handler-registry redesign (`cistella:todos/44`).

## Capabilities

### New Capabilities

- `tempdir-service`: per-session `/tmp` lifecycle — host-slice mint, RW triple contribution, teardown reap; vendoring and registry shape for the reference extension.

### Modified Capabilities

- `extension-protocol`: prepare request carries the session id; extension mount contributions (including RW) merge through the central validators instead of blanket-refusing.
- `src-confinement`: declared RW targets compose as FULL carveouts including an admitted `/tmp` slice.
- `framework-lifecycle`: teardown covers extension-contributed host state; hook slots for the reap duty.

## Impact

- `src/framework/prepare.rs` (payload + RW admission removal), wire request shape (additive session-id field), hook argv composition (`src/framework/hooks.rs` + wrapper policy: FULL grant on the admitted slice), `src/runtime.rs` + teardown paths (reap contributed dirs), new extension binary (vendoring: separate build artifact resolved via registry — Dogfood path, not a shipped sibling), `documentation/usage/profiles.md` (tempdir profile shape), conformance suite (RW-contribution pins, session-id pins, reap pins). Operator decision recorded: if tier-2 moves to neuter the RW admission, escalate to the operator before complying.
