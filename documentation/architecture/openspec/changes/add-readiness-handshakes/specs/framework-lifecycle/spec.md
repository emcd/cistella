## ADDED Requirements

### Requirement: Dedicated state query verb

The host CLI SHALL provide `describe state` reporting launch commitment plus a backend snapshot. The verb SHALL resolve ownerlessly and report: a commitment bit (attested with attempt provenance, or unattested) and — only where backend provenance exists — a snapshot enum from created/initiated/stopped/absent in verbatim lowercase. The verb SHALL NEVER report `executing` (not ownerlessly provable; reserved to the in-protocol scoped meaning). Attested+running SHALL read `initiated`. Backend provenance rule: an attestation carrying an incarnation proves the publisher was native (backend fact, stable); live incarnation equality proves currency (the same incarnation still runs — checked per read). Snapshot rendering additionally requires currency. Unincarnated commitment with no record proves nothing locally (unsupported); a record with no attestation proves nothing about backend (explicitly `backend=unknown` with NO snapshot — the upgrade-window cost). Accepted limit, recorded not waived: a pre-change foreign session (no record, no attestation, no container) is indistinguishable from a typo'd id under a native-scoped probe, and resolves `absent` exactly like one — not global-absence knowledge, only the probe's local result. Selection: no-match in any form keeps the existing typed Selector error EXCEPT a FULL session id matching no record, which probes the derived container name and follows the residue table with query scope marked (`scope=native`, never a global `backend=` claim). Podman-specific facts SHALL be gated to proven-native sessions. Record + unincarnated commitment (contradictory provenance — foreign conducts create no unit files) SHALL refuse typed, never render. Default `inspect` SHALL remain byte-identical.

#### Scenario: State reports commitment and snapshot

- **WHEN** a session's attestation is committed and its container runs
- **THEN** `describe state` reports `initiated` plus attested commitment with success exit — never `executing`

#### Scenario: State is postmortem-safe

- **WHEN** the owner is dead and the unit file persists (orphan, container possibly still running)
- **THEN** `describe state` reports its actual snapshot (commitment + backend facts as observed) with success exit, never a contact error

#### Scenario: Foreign no-record probe is unsupported

- **WHEN** a full-id probe matches no record, no container, and no unit file, but an unincarnated (foreign) commitment is present
- **THEN** `describe state` fails typed as unsupported rather than inferring foreign absence from local Podman queries alone

#### Scenario: Unattested record reports unknown backend without snapshot

- **WHEN** a record matches but no attestation is committed (pre-change session or pre-commit window)
- **THEN** `describe state` reports commitment=unattested and backend=unknown with success exit, and renders no lifecycle snapshot

#### Scenario: Contradictory provenance refuses typed

- **WHEN** a record matches an unincarnated (foreign) commitment (which production never produces — foreign conducts create no unit files)
- **THEN** `describe state` fails typed naming the contradiction, never rendering any shape

### Requirement: Residue and selection truth table

Full-id probes with no record SHALL resolve by backend evidence in this exact order: container OR unit-file present (any state) → typed residue error naming the container and directing to terminate/gc (backend fact dominates registry absence); else attestation-only residue → `absent` with success exit and query scope marked (`scope=native`, never a global `backend=` claim; inert metadata, gc reaps); else all absent → `absent` with success exit and `scope=native`; backend query failure during the probe → typed observation error. Fuzzy (prefix/directory/label) no-match and multi-match SHALL keep the existing Selector errors. The probe exists because record-absence is expected post-teardown; no invariant that live sessions match records SHALL be assumed.

#### Scenario: Live residue dominates record absence

- **WHEN** no record matches but the derived container or unit file exists (running, stopped, or file-only)
- **THEN** `describe state` fails typed naming the residue container; it never reports `absent`

#### Scenario: Inert attestation residue resolves Absent

- **WHEN** no record and no container exist but attestation residue remains
- **THEN** `describe state` reports `absent` with success exit

#### Scenario: All absent resolves Absent

- **WHEN** a full id matches nothing anywhere (unit file, container, attestation)
- **THEN** `describe state` reports `absent` with success exit

#### Scenario: Probe query failure is explicit

- **WHEN** the backend cannot be contacted during a full-id probe
- **THEN** `describe state` fails with a typed observation error, never an `absent` claim

### Requirement: Owner-written launch attestation

The conduct owner SHALL capture the native incarnation (container ID + StartedAt) BEFORE `execute_launch` (pre-capture; container absent → refuse+converge) and re-query immediately after Ok (post-check); the file SHALL publish ONLY the pre-verified identity when pre==post exactly — any mismatch or unestablishable identity SHALL refuse + converge the attempt via typed teardown with nothing published. The file SHALL carry session id, container name, attempt reconciliation key, harness argv, fw execution handle, and backend incarnation (container ID + StartedAt queried at commit). All file I/O SHALL follow the anchored state-root discipline (uid-scoped state dir opened O_NOFOLLOW|O_DIRECTORY with seat-or-root ownership check and 0700 hardening; file opens NOFOLLOW with fstat seat-ownership; atomic temp+link publication with EEXIST-preserving write-once — linkat, since renameat would clobber — the `cleanup.rs`/`guest_anchor.rs` pattern). Publication is atomic as observed by processes (readers see absent-or-complete); power-loss/host-crash durability is NOT promised — a crash may leave a corrupt or missing commitment (fail closed typed) or staging litter (gc ignores non-matching names; harmless). Binding checks: id ↔ filename + record; container ↔ record + presence; incarnation ↔ live re-query (exact equality); key/argv/handle are write-time bindings and read-time provenance (not re-checked). Write failure after Ok SHALL converge the attempt via typed teardown and report the failure. Container-name mismatch → typed mismatch error. Incarnation mismatch → typed stale-generation error (identity-unknown → refuse, never ready). Corrupt/unreadable with present record → typed corruption error. Missing file → unattested. Record-gone → Absent path; teardown removes the unit file before reaping the attestation; gc reaps orphans. Pre-change sessions without files read unattested (documented window). A pre-commit visible child is not acknowledgement.

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
- **THEN** `describe state` reports unattested with a success exit (missing is the normal precommit state, not an error), while `enter` refuses with a typed readiness error and nonzero exit

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
