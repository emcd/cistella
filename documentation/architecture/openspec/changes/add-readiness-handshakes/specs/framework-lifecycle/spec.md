## ADDED Requirements

### Requirement: Dedicated state query verb

The host CLI SHALL provide a dedicated `state` verb reporting launch commitment plus a backend snapshot. The verb SHALL resolve ownerlessly and report: a commitment bit (attested with attempt provenance, or unattested) and a snapshot enum from created/initiated/stopped/absent in verbatim lowercase. The verb SHALL NEVER report `executing` (not ownerlessly provable; reserved to the in-protocol scoped meaning). Attested+running SHALL read `initiated`. Selection: no-match in any form keeps the existing typed Selector error EXCEPT a FULL session id matching no record, which probes the derived container name and follows the residue table. Podman-specific facts SHALL be gated to Podman-backed sessions. Foreign sessions SHALL report commitment + record metadata verbatim + explicit admittability-unsupported (never an invented snapshot). Default `inspect` SHALL remain byte-identical.

#### Scenario: State reports commitment and snapshot

- **WHEN** a session's attestation is committed and its container runs
- **THEN** `state` reports `initiated` plus attested commitment with success exit — never `executing`

#### Scenario: State is postmortem-safe

- **WHEN** the owner is dead and the unit file persists (orphan, container possibly still running)
- **THEN** `state` reports its actual snapshot (commitment + backend facts as observed) with success exit, never a contact error

#### Scenario: Foreign no-record probe is unsupported

- **WHEN** a full-id probe matches no record and the session would be foreign-registered (backend identity not establishable from local evidence)
- **THEN** `state` fails typed as unsupported rather than inferring foreign absence from local Podman queries alone

#### Scenario: Foreign state marks admittability unsupported

- **WHEN** the session is foreign-registered
- **THEN** `state` reports commitment and record metadata verbatim plus admittability-unsupported; no container/pid observation is asserted and no lifecycle snapshot is invented

### Requirement: Residue and selection truth table

Full-id probes with no record SHALL resolve by backend evidence in this exact order: container present (any state) → typed residue error naming the container and directing to terminate/gc (backend fact dominates registry absence); else attestation-only residue → `absent` with success exit (inert metadata; gc reaps); else all absent → `absent` with success exit; backend query failure during the probe → typed observation error. Fuzzy (prefix/directory/label) no-match and multi-match SHALL keep the existing Selector errors. The probe exists because record-absence is expected post-teardown; no invariant that live sessions match records SHALL be assumed.

#### Scenario: Live residue dominates record absence

- **WHEN** no record matches but the derived container exists (running or stopped)
- **THEN** `state` fails typed naming the residue container; it never reports `absent`

#### Scenario: Inert attestation residue resolves Absent

- **WHEN** no record and no container exist but attestation residue remains
- **THEN** `state` reports `absent` with success exit

#### Scenario: All absent resolves Absent

- **WHEN** a full id matches nothing anywhere (unit file, container, attestation)
- **THEN** `state` reports `absent` with success exit

#### Scenario: Probe query failure is explicit

- **WHEN** the backend cannot be contacted during a full-id probe
- **THEN** `state` fails with a typed observation error, never an `absent` claim

### Requirement: Owner-written launch attestation

The conduct owner SHALL capture the native incarnation (container ID + StartedAt) BEFORE `execute_launch` (pre-capture; container absent → refuse+converge) and re-query immediately after Ok (post-check); the file SHALL publish ONLY the pre-verified identity when pre==post exactly — any mismatch or unestablishable identity SHALL refuse + converge the attempt via typed teardown with nothing published. The file SHALL carry session id, container name, attempt reconciliation key, harness argv, fw execution handle, and backend incarnation (container ID + StartedAt queried at commit). All file I/O SHALL follow the anchored state-root discipline (trusted-parent FDs, O_NOFOLLOW, fstat seat-ownership, O_EXCL publication, dev+ino re-verification — the `cleanup.rs` pattern). Binding checks: id ↔ filename + record; container ↔ record + presence; incarnation ↔ live re-query (exact equality); key/argv/handle are write-time bindings and read-time provenance (not re-checked). Write failure after Ok SHALL converge the attempt via typed teardown and report the failure. Container-name mismatch → typed mismatch error. Incarnation mismatch → typed stale-generation error (identity-unknown → refuse, never ready). Corrupt/unreadable with present record → typed corruption error. Missing file → unattested. Record-gone → Absent path; teardown removes the unit file before reaping the attestation; gc reaps orphans. Pre-change sessions without files read unattested (documented window). A pre-commit visible child is not acknowledgement.

#### Scenario: Attestation commits with bound incarnation

- **WHEN** `execute_launch` returns Ok and the post-check incarnation equals the pre-capture
- **THEN** the file binds id, container, key, argv, handle, container ID, and StartedAt before any readiness check can pass

#### Scenario: Launch-interval recreate refuses and converges

- **WHEN** the incarnation changes anywhere inside the pre-capture to post-check interval (or either query is unestablishable)
- **THEN** no attestation is published and the attempt converges via typed teardown reporting incarnation-refusal; the replacement never inherits the old attempt's commitment

#### Scenario: Same-name recreate refuses stale

- **WHEN** the container name was recreated or restarted after commit (ID or StartedAt differs) with the old attestation present
- **THEN** readiness reads fail typed as stale-generation; the old commit never readies the new incarnation

#### Scenario: Missing is unattested success for state, typed refusal for enter

- **WHEN** no attestation file exists
- **THEN** `state` reports unattested with a success exit (missing is the normal precommit state, not an error), while `enter` refuses with a typed readiness error and nonzero exit

#### Scenario: Corruption fails loud

- **WHEN** the file is corrupt/unreadable with a present record
- **THEN** readers fail typed, never silently demoting

#### Scenario: Record-gone dominates inert residue

- **WHEN** the record is gone but attestation residue remains (no container)
- **THEN** resolution follows the Absent path; gc reaps the residue

### Requirement: Enter gated on launch readiness

`enter` SHALL NOT proceed on service-active alone, SHALL NOT retry, and SHALL NOT poll. In a single bounded pass for Podman-backed sessions it SHALL require ALL of: committed attestation matching session + container name; incarnation equality with the live container; container State.Running; unit file present; init PID > 0. The edge proves commitment + admittability, never "this exec will succeed": post-check exec failure stays honest typed error. Unmet conditions refuse immediately typed. The readiness guarantee is native-Podman-scoped: foreign `enter` keeps the legacy direct-exec path bit-for-bit with no gate and no readiness claim (unsupported proof explicitly refused).

#### Scenario: Enter refuses before commit

- **WHEN** the service is active but no attestation is committed
- **THEN** `enter` refuses immediately typed; no retry occurs

#### Scenario: Enter refuses stale incarnation

- **WHEN** the attestation is committed but the live container ID or StartedAt differs
- **THEN** `enter` refuses typed as stale-generation, even with the container running

#### Scenario: Enter refuses stopped containers

- **WHEN** attested but the container is stopped or exited
- **THEN** `enter` refuses typed; stopped presence never readies exec

#### Scenario: Foreign enter keeps legacy path

- **WHEN** the session is foreign-registered
- **THEN** `enter` execs directly as today with no readiness gate and no readiness claim

### Requirement: Per-unit executing scope

`PodmanIsolator::inspect` SHALL report `executing` only when the inspected unit itself has an unreaped harness child (`pending.unit == *handle`), never when another unit's child is live. The `classify_state` per-unit docstring SHALL hold in code.

#### Scenario: Other unit executing does not ready this unit

- **WHEN** unit A has a live harness child and unit B has none, with both containers running
- **THEN** `inspect` on B reports `initiated`, not `executing`
