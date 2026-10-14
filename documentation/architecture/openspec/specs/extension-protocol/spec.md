# extension-protocol Specification

## Purpose
Framework vertical slice (0.2.0): synced from change framework-isolators-extensions. Update Purpose after archive.
## Requirements
### Requirement: Versioned stdio protocol with framing

External isolators and extensions SHALL communicate over stdio with versioned JSON messages in length-prefixed frames. Header layout: 4-byte unsigned big-endian frame length followed by the JSON envelope bytes; the length counts exactly the envelope bytes (no header, no trailer). Envelopes SHALL be UTF-8 JSON objects with `protocol` (major version integer for bootstrap), `id` (request correlation), `op`, and `payload`; unknown fields refuse deterministically. Request IDs are per-connection unique opaque strings chosen by the requester; responses SHALL echo the request `id`; unknown or duplicate responses fail the exchange, with one narrow exception: `isolator.await_result` MAY emit zero or more `{pending}` frames followed by exactly one terminal frame under the same echoed `id`, and no frames after terminal; unrelated IDs/ops are still refused. Cancelling or re-attaching closes the old stream: the framework marks the prior channel dead, and only frames on the live channel redeem; two connections SHALL NOT redeem conflicting terminal responses. IDs carry no durability beyond one connection. The first hello frame arrives before negotiation, so an independent pre-negotiation ceiling of 64 KiB applies to it regardless of the negotiated maximum (which may vary); truncation or EOF mid-frame fails the exchange; length exceeding the applicable maximum rejects before allocation. Parser library may defer to implementation. The exchange carries request IDs, protocol/version negotiation, capability sets, discarded protocol stderr (structured diagnostics use the dedicated channel), and pinned executable discovery (never PATH-searched). A `hello` exchange with capability advertisement opens every session; mismatches refuse fail-closed before any planning.

#### Scenario: Version mismatch refuses before planning
- **WHEN** a guest advertises an unsupported protocol version
- **THEN** the framework refuses with a typed version error before capability negotiation completes

#### Scenario: Stdout purity enforced
- **WHEN** a guest emits non-protocol bytes on stdout
- **THEN** framing validation fails the exchange with a typed protocol error; guest logs are only read from stderr

### Requirement: Prepare transaction with central merge

Extensions SHALL return at most one prepare transaction per session with separately-typed environment sets, placement mount sets, rights permission sets, policy claims, guest-hook requests, and cleanup declarations (absolute host paths the extension minted and owns, default empty). Mount contributions declare placement only (`mounts[]` as `(host_source, container_target)` — modeless); rights travel separately (`permissions[]` as `(target, rights)` pairs where rights is exactly one of the wire strings `read`, `write`, `full`; bare `write` normalizes to effective `full` before every consumer with original labels preserved for audit). Permission labels are minimum grants; original binding intent travels as separate enforcement-bearing state per target (most restrictive declared binding wins for retention and revision). The protocol epoch moves 1→2 for this incompatible response change — one authoritative discriminator shared by the whole wire (hello precedes everything for isolator and extension guests alike; no second per-shape mechanism). Host and ALL guest-role binaries speak epoch 2; no legacy-1 constant remains in any guest-role type. The version gate is checked before body parsing: bounded envelope/framing parses to read the version while incompatible operation payloads never decode. Mismatch refuses deterministically typed in both directions with no prepare dispatch, no state minting, and no compile. Pre-1.0 means a sharp break stated plainly — no translation shim, no binary harness; coverage is hello-layer unit tests in both refusal directions using actual legacy-v1 against epoch-2 with deliberately incompatible bodies (operator directive 2026-10-14; the lost legacy-binary integration coverage is stated on the record, not silently dropped). The additive session-id request tolerance below covers the session-id field only and justifies nothing about version breaks. The prepare request SHALL carry the opaque session id minted for the session (containers and extension state share one identity); extensions SHALL treat it as opaque (never parse it) and MAY use it to mint per-session host state. The framework SHALL validate each set with its existing rules, merge centrally in spine order, and evaluate policy claims against the severity × scope lattice (tighten-only downward across site > user > defaults). Extension mount contributions SHALL merge through the central validators like every other set (operator direction: declarations are authoritative intent; the framework enforces them, it does not second-guess them); carveout derivation reads the permission sets, never placement fields. A contributed placement with no matching permission refuses typed. A permission target is addressable iff it matches (a) a placement contributed by ANY guest in the same session, (b) a driver/profile/CLI/worktree target, or (c) a known image-private baseline target (guest `/tmp` and `/dev` — the fixed FULL baselines, which need no placement); a permission matching none of these refuses typed as an unknown grant target. There is no silent default and the namespace never widens silently. Cleanup declarations SHALL be absolute paths (relative refuses); only declarations are deletion authority. This spec owns claim expression over stdio; lattice semantics live in the framework-lifecycle spec. Extension output is untrusted input at every step.

#### Scenario: Policy claim tightens only
- **WHEN** an extension claims a constraint weaker than the applicable site/user rule with otherwise valid shape
- **THEN** the framework discards the weakening and enforces the stricter rule with a typed diagnostic; the transaction proceeds

#### Scenario: Invalid policy claim refuses whole
- **WHEN** a `policy_claim` is malformed, overreaches its transaction, or carries unknown fields
- **THEN** the framework refuses the entire prepare transaction with a typed error; "typed error but continue" is not an outcome (atomicity holds)

#### Scenario: Guest-hook requests are explicit
- **WHEN** an extension requests guest pre-exec presence (e.g. a Landlock wrapper as exec ancestor)
- **THEN** the framework records the hook with its dedicated diagnostics channel; hooks never emit on protocol stdout nor hijack harness stdio

#### Scenario: Prepare carries the session id
- **WHEN** an extension answers prepare
- **THEN** the request names the opaque session id; the guest MAY mint per-session host state keyed by it and MUST NOT parse it

#### Scenario: Old decoders tolerate the session id (historical REQUEST pin only)
- **WHEN** a guest built against the pre-session-id request shape answers a prepare carrying the field, in an isolated legacy REQUEST-decoding pin (not an end-to-end session against the current host)
- **THEN** the request decodes (pinned against an actual old guest binary); the major stays unchanged for that field — this scenario justifies nothing about the permissions response break, where old guests refuse deterministically per the version scenarios above

#### Scenario: Read-write mount contribution merges
- **WHEN** an extension contributes a read-write placement with otherwise valid shape (topology validates, no collision, lattice evaluates)
- **THEN** the merged plan carries it like any declared triple; no blanket read-write refusal exists

#### Scenario: Rights travel separately from placement
- **WHEN** an extension contributes a placement plus its rights permission for the same target
- **THEN** validators check placement topology while carveout derivation reads the permission set; placement fields never decide rights

#### Scenario: Placement without permission refuses
- **WHEN** an extension contributes a placement with no matching permission for its target
- **THEN** the transaction refuses typed; no default rights are assumed and nothing is silently widened

#### Scenario: Orphan permission refuses
- **WHEN** an extension contributes a permission matching no contributed placement, no driver target, and no known image-private baseline
- **THEN** the transaction refuses typed as an unknown grant target; dangling rights never enter the merge

#### Scenario: Cross-guest permission matches another placement
- **WHEN** landlock declares `Full` on `/tmp` while tmpdir contributes the `/tmp` placement (or on image-rootfs `/tmp` with no placement at all)
- **THEN** the permission is addressable (another guest's placement, respectively the known baseline) and joins normally; each side passed its own per-plan gate

#### Scenario: Version mismatch refuses old guest
- **WHEN** a legacy-v1 hello carrying a deliberately incompatible mode-bearing prepare body meets an epoch-2 host
- **THEN** the version gate refuses deterministically with a VERSION error naming expected vs offered version (asserted: version refusal, never shape/serde error); no body decodes and no prepare dispatches, mints, or compiles

#### Scenario: Version mismatch refuses old host
- **WHEN** an epoch-2 guest answers a legacy-v1 host that never offered version 2
- **THEN** the guest refuses deterministically before contributing (unit-pinned at the hello layer); old and new never half-merge

### Requirement: Guest-hook delivery schema

Guest-hook delivery SHALL be constructible now, not delegated to a later contract: admitted artifact reference kinds (content-pinned executable blobs by digest, isolator-staged paths); staging owner (the isolator stages artifacts inside the guest before initiate completes); deterministic wrapper composition order (framework-declared sequence, single wrapper chain, no competing wrappers); argv composition (wrapper argv owned by the framework plan, harness argv appended verbatim); probe/apply messages (guest-context capability probe after initiate, apply confirmation before execute); diagnostics FD ownership and closure (framework-allocated FD, closed on wrapper exit, disjoint from protocol stdout and harness PTY stdio); launch/result messages through the wrapper; and failure cleanup (failed staging or probing removes the artifact and fails pre-exec). Exit/result propagation flows through the wrapper with the wrapper's own status distinguishable from the harness status.

#### Scenario: Wrapper order deterministic
- **WHEN** multiple guest hooks apply to one session
- **THEN** they compose in framework-declared sequence; competing or unordered wrappers refuse at merge time

#### Scenario: Diagnostics FD disjoint
- **WHEN** a wrapper emits diagnostics during execute
- **THEN** bytes arrive on the framework-allocated FD only; protocol stdout carries framing and harness stdio carries the session with no interleaving possible

### Requirement: Guest-hook delivery schema (continued)

Hook request payload (inside the prepare response) SHALL conform exactly to this shape, and deviations SHALL refuse with a typed error: `{artifact: {kind: digest-pinned-blob, sha256, source: {registry, path}}, staging: isolator-staged, order: uint32, argv_prefix: [...], probe: {op, timeout_ms}, on_failure: fail-pre-exec}` — note no diagnostics FD: the extension never selects channel numbers. Artifact bytes come from the framework-named registry/path in `source`; the isolator verifies the digest before staging and refuses mismatch pre-initiate. The framework assigns the diagnostics channel in the accepted-plan response (`{diagnostics_channel}` opaque handle) and passes it across the isolator boundary at launch; the wrapper writes diagnostics there and the framework closes it on wrapper exit. Probe/apply messages: guest-context capability probe after initiate (`probe_capabilities` → capability set), apply confirmation before execute (`apply_restrictions` → applied attestation). Launch/result messages flow through the wrapper: `launch` carries the harness argv appended verbatim after `argv_prefix`; `result` separates wrapper status from harness status. Diagnostics FD is framework-allocated, closed on wrapper exit, disjoint from protocol stdout and harness PTY stdio by construction.

#### Scenario: Probe failure fails pre-exec
- **WHEN** guest-context capability probing reports a required restriction unsupported
- **THEN** apply never runs; conduct fails pre-execute with a typed capability error and staged artifacts are removed

### Requirement: Host preparers vs guest pre-exec separation

Host helpers SHALL run pre-initiate and return plans only. Guest components that must inherit into the harness (Landlock restrictions and their descendants) SHALL be established as the execute ancestor/wrapper with lifecycle and diagnostics owned by guest execution mechanics. Guest-hook delivery SHALL define: artifact provenance with identity pinning; how the isolator stages the artifact inside the guest; guest-context capability probing with apply after initiate; wrapper composition with deterministic ordering; path and argv ownership; a dedicated diagnostics transport disjoint from protocol stdout and harness PTY stdio; failure cleanup; and exit/result propagation through the wrapper. The Landlock spike SHALL prove helper placement, exec ancestry, and Podman/user-namespace rule preservation with three-way assertions (admitted succeeds, denied fails with `EACCES`, unsupported yields a typed pre-execute `Unsupported` capability result — never an errno pin) before the protocol freezes.

#### Scenario: Landlock three-way proof
- **WHEN** the Landlock extension runs its conformance surface
- **THEN** admitted access succeeds, denied access fails with `EACCES`, and unsupported kernels yield a typed pre-execute `Unsupported` capability result (distinct from successful confinement, never satisfying a denial assertion)

### Requirement: Credential seam with constrained opaque-handle invariant

Credential contributions SHALL use strict handle variants only: each admitted kind declares a bounded locator grammar with a namespace, framework-owned resolution locality, and unknown-field rejection. Handles SHALL be framework-issued opaque identifiers or registry-resolved against a pre-existing framework-owned registry — never user-chosen strings, so no secret can hide in an arbitrary permitted string. Locator sensitivity is explicit per kind with diagnostic redaction rules; diagnostics name the handle kind and locator class, never content. Raw extension protocol stderr is discarded in 0.2.0 (never stored, never forwarded, never rendered): structured guest diagnostics flow only through the dedicated diagnostics channel. The JSON schema provides no designated secret-value channel. Each admitted handle kind requires separate tier-2 review of its resolution path. Capability advertisement for the seam is proven by the deterministic fake (forbidden shapes refused, admitted handles accepted, oversized strings refused), not by real credential transport.

#### Scenario: Value-bearing credential contribution refused
- **WHEN** a guest returns a credential contribution containing a value-shaped field
- **THEN** schema validation refuses the transaction with a typed error before merge
