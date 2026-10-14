## Why

`credential-surface` is bespoke driver-assembled wiring predating extensions, and a second selection path competing with the extension table. Pre-1.0 we break sharply: delete the slot entirely and let a standalone shipped ssh extension do the job — declared like any extension, fed by seat ambient, reviewed like every guest. This change also supplies the first production triple (landlock+tmpdir+ssh), the proof the N-law still owes.

## What Changes

- **BREAKING**: the profile `credential-surface` slot is deleted (field, `CredentialSurface` enum, `identity::ssh_agent_volume_args`, the `main.rs` join block, the acceptance-gate exception). Profiles naming it refuse at parse. All baked, test, and fleet profiles are migrated: agent forwarding becomes `[[extensions]]` `ssh` or nothing.
- New shipped guest binary `cistella-extension-ssh` (role `ssh`, autobin beside the driver): hello advertises `environment`, `mounts`, `permissions`, `credentials`; one prepare transaction contributes the `SSH_AUTH_SOCK` pointer env, the socket bind read-only, a `read` permission, and the `seat-socket` credential handle. Hooks-free — composes under the N-law without touching the single-wrapper constraint.
- Selection is declaration-only: `[[extensions]]` `ssh` resolves shipped first (landlock/tmpdir pattern — `ExtensionResolution::Ssh`, sibling install-dir trust). No intent slot, no auto-spawn, no special case: undeclared means no agent, exactly like every other guest.
- The socket path comes from seat ambient: the driver reads invoker `SSH_AUTH_SOCK` at selection, canonicalizes it (P declared-visible value, C canonical authority), and passes `--socket=P --socket-canonical=C`. Ambient alone never injects — declaration forwards ambient's value. Declared ssh with unset/unreadable ambient refuses (declared-but-unprovidable, fail-closed).
- Ordering is declaration order like every guest — no ssh-last rule, no position privilege. Same-target collisions refuse naming all claimants.
- Registry entries named `ssh` refuse at parse (shadowing refusal, landlock pattern). `:` is refused in all registry entry names (namespaced instance identities — see design).
- Non-goals: key generation, agent lifecycle, server-in-container, per-host identity policy, advisory policy warnings. Forwarding of the seat's agent socket, nothing more.

## Capabilities

### New Capabilities

- `ssh-service`: the shipped ssh guest — declaration, ambient sourcing, prepare contribution shape (env P, mount C→P, permission P, credential C), refusal conditions (bad epoch/op, missing session, unset/non-socket ambient, divergent bundle).

### Modified Capabilities

- `identity`: the agent-socket wiring mechanism moves from `credential-surface` driver assembly to declared-extension prepare contribution. Observable session behavior for migrated profiles is unchanged (parity); the slot and its machinery are gone.
- `guest-hosting`: `ssh` resolves shipped like `tmpdir`; guest owner identities become host-unique namespaced instance IDs (`shipped:<selection>` / `registered:<entry>`, `:` refused in entry names).

## Impact

- New: `src/bin/cistella-extension-ssh.rs`; delta specs `ssh-service`, `identity`, `guest-hosting`.
- Modified: `src/profile.rs` (slot deletion, `ExtensionResolution::Ssh`), `src/identity.rs` (helper deletion), `src/main.rs` (ambient selection, join-block removal), `src/framework/prepare*.rs` (spawn, instance IDs, slot ownership, locator equality), `src/guest_registry.rs` (`ssh` shadowing + `:` refusal), all profiles carrying the slot, unit/integration tests (parity pins, real-triple pins).
- Deleted: `credential-surface` syntax. Fleet profiles migrate out-of-band.
- Wire protocol: untouched (pure epoch-2 consumer). No version bump.
