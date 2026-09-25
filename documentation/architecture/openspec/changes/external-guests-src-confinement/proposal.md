## Why

The 0.2.0 framework half landed the contracts, the lattice, and fixture-only guests — but `conduct` still drives Podman in-process and spawns no extension, so the isolator/extension architectures are unproven where it matters: across a process boundary with real confinement. The fleet cannot take 0.2.0 until `~/src` is safe to mount broadly, and safety must be demonstrated, not asserted.

## What Changes

- Podman isolator as a real external guest: separate `--bin` in the same crate, speaking the versioned stdio protocol (`isolator.*` ops), discovered/pinned by the framework, never PATH-searched. `conduct` drives lifecycle over the wire; the in-process trait impl becomes the conformance reference, not the production path.
- Landlock extension as a real external guest: separate `--bin` in the same crate, answering `prepare` with mount/env contributions plus a guest-hook request, delivering the wrapper as exec ancestor with probe/apply and the dedicated diagnostics channel.
- `~/src` confinement as the demonstration gate: the project subtree mounts RW while Landlock denies writes everywhere else under the translated guest ancestor route (union semantics: R+X on the guest ancestor, full rights on the guest project subtree; host `~/src/<project>` or `~/src/CLONES/<project>/<lane>` is the translation source). Proof is denial evidence from attempted writes in conformance, not reviewed intent.
- Guest-hosting mechanics in the framework: pinned binary discovery, stdio hosting with framework-owned deadlines, kill/reap ownership, and reconciliation across the wire (handles stay framework-issued opaque strings).
- No fleet migration in this change: 0.1.x profiles run unchanged; confinement binds per-seat at deploy time after the demonstration passes.

## Capabilities

### New Capabilities

- `guest-hosting`: discovery/pinning of external guest binaries, stdio hosting, deadline/kill/reap ownership, wire-level reconciliation identity.
- `src-confinement`: Landlock-delivered subtree confinement of the worktree ancestor with denial-evidence proof obligations.

### Modified Capabilities

- `isolator-contract`: external operation wire schemas become load-bearing (exercised by the real Podman guest, not just the fake peer); in-process impl retained as conformance reference.
- `framework-lifecycle`: prepare and guest-hook phases now cross into real external guests (deadlines, probe/apply, diagnostics channel carry live traffic).

## Impact

- `src/` gains two `--bin` targets plus framework hosting paths in `conduct`; `Cargo.toml` example/peer rationale extends to shipped guest binaries (packaging: `package.include` must cover the new bins).
- Live proof burden falls on the tandem QA seat again (podman/systemd/userns); Owner seat stays containerized.
- Fleet deploy of 0.2.0 stays blocked until the confinement demonstration passes — this change is the gate.
