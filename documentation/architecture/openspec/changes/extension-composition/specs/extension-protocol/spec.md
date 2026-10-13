# extension-protocol Specification

## Purpose
Extension protocol: synced from change extension-composition. Update Purpose after archive.
## MODIFIED Requirements
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
