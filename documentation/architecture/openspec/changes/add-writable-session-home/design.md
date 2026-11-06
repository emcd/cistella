## Context

Session home is always a bare `podman --tmpfs` today (`cli_conduct.rs` passes `None` for `session_home_source`; quadlet renders `Tmpfs=`). The mountpoint preparation walk stops at the home root and the grant set contains no home rule, so the home target falls through to `/` R+X: readable and traversable, never writable (cistella:issues/26). The `src == "tmpfs"` spelling already exists in `podman_volume_args` but nothing feeds it a home source. FULL-route accounting (`full_grant_routes` / `full_sources`) plus the RO-retention revision and contradiction preflight already solve "RO stays RO under FULL" for mount subtrees; home is simply absent from that accounting.

## Goals / Non-Goals

**Goals:**

- A writable session home for container-private (tmpfs) homes with no RO weakening.
- An explicit, machine-checkable declaration of home provenance that keeps a future host-backed home fail-closed.

**Non-Goals:**

- Implementing host-backed homes (declared, not built).
- Changing RO-retention or preflight semantics; changing transport, wire, or registry behavior.
- Migrating home into a plain `[[mounts]]` triple (ordering, HOME derivation, and unit rendering keep it special).

## Decisions

- Field `home-source-class = "container" | "host"` beside `container-home` (general, not isolator-specific: both isolators already consume `container-home`), plus `home-host-source` path required exactly when class is `host`. Rejected `home-source`: confusable with a path value. Rejected home-as-triple: larger blast radius across ordering, env derivation, and both unit renderers for no user-visible gain.
- Absent field means container-private (the only constructible topology; accurate default, not presumption). The default is safe only because construction validation proves the private topology (see matrix below) — the declaration never substitutes for backing validation.
- One authoritative home descriptor (canonical target + class + backing), constructed once at validation and bound into the compile snapshot as a fingerprinted field: `CompileSnapshot` gains the descriptor, `fingerprint_snapshot` covers it, and the supervisor FULL derivation, the RO-retention sets, and the actual home mount rendering all consume that same descriptor. The current split — compiler trusting profile values while `cli_conduct.rs` plans the wrapper and calls `podman_volume_args(..., prof.home(), None)` on an independent path — is closed by threading the descriptor into the volume-args home source (replacing the hard `None`) so compiler, accounting, and renderer cannot disagree about the actual backing. Snapshot/stale-plan consistency is structural: `recompute_fingerprint` recomputes from current contents, so a descriptor mutation cannot reuse a stale fingerprint, and a rendering from any other snapshot refuses at application.
- Admission authority and render derivation (not just consistency): the construction-capable descriptor is minted by the driver's admitted construction validator as a closed driver-owned type (typed class/backing variants; private validated construction or equivalent — no particular Rust layout imposed). Compile-only host fixtures are NOT launch authority and MUST NOT reach create. Final create/render MUST NOT use any independently supplied home source, `session.container_home`, raw volume list, or reconstructed profile value to substitute a different target or backing after admission: all home inputs derive exclusively from the admitted descriptor or are verified equal to it before construction. The consumer audit covers every home input, not just the volume-args `None`: podman volume args, Quadlet `Tmpfs=` rendering (which today reads `session.container_home`, quadlet.rs:295-305), HOME env derivation, and supervisor rule emission must all reach the same admitted target/backing identity. Existing final topology validation MUST still reject profile/CLI/extension mounts at HOME or an ancestor that would shadow the private root, retaining admitted descendants and the existing FULL-source alias safeguards. Trust boundary: this closes substitution within driver-owned inputs; it claims no protection against an arbitrarily malicious container engine or in-process code.
- Compiler grants FULL on the home target exactly when the descriptor says container-private, and folds the route into `full_grant_routes` (no host source enters `full_sources`: tmpfs contributes none). RO triples under home keep `:ro` bindings through the unchanged retention machinery; RW-beneath-RO under home hits the existing preflight.
- Construction matrix pinned by executable tests (backend-free validation pins plus a construction gate): absent/container class with no host source constructs private tmpfs; any host source with absent/container class is a typed refusal; host class without a path is a typed refusal; host class WITH a path is an explicit unsupported-construction refusal before unit or scratch creation in this release; unknown class values are typed refusals. Host-backed homes are not implemented here.

## Risks / Trade-offs

- Default-FULL is an observable relaxation for Landlock-enabled profiles (described as such in the changelog, not as a harmless fix): accepted, because unwritable home was emergent, never a contracted guarantee — the main mounts spec already calls session-home a distinguished root and states no unwritable invariant; the behavior change is why this rides a minor release.
- Compatibility: under a newly FULL home, an RO parent with an RW descendant that previously worked via RO-to-RW materialization outside FULL now hits the retained-RO contradiction preflight. Documented remedy: move the graft out from under the RO dir. Existing denial semantics are kept, not weakened.
- A blanket home rule widens every unmounted home subtree to FULL: bounded, because the home tmpfs is container-private and ephemeral, and host reachability still passes per-mount modes plus grants.
- Grant bugs are silent-allow risks: mitigated by backend-free pins on the grant set and snapshot fingerprint coverage (home FULL present for container class, absent for host/absent-misuse; descriptor mutation invalidates the fingerprint) plus live proof (writable home, RO stays RO) before merge.
