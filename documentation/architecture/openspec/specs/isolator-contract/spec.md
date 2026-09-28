# isolator-contract Specification

## Purpose
Framework vertical slice (0.2.0): synced from change framework-isolators-extensions. Update Purpose after archive.
## Requirements
### Requirement: Isolator interface with declared capabilities

Isolators SHALL implement `create` / `initiate` / `execute` / `inspect` / `state` / `terminate` / `remove` as distinct external operations. `inspect` returns a rich read-only snapshot (configuration, detailed status); `state` returns the lifecycle-state enum only (created/initiated/executing/stopped/absent); every state transition remains a separate semantic operation. The internal Rust trait may share an enum-bearing method. Each operation declares preconditions and postconditions, typed resource handles, partial-success reporting, and exit/status delivery. The framework SHALL own lifecycle sequencing and idempotent teardown semantics; isolators SHALL own runtime mechanics. Teardown SHALL be safely repeatable (removing an absent unit succeeds). After an uncertain mutating outcome (timeout with unknown applied state), the isolator SHALL support recovery by reconciliation: re-inspect, report actual state, and converge to either applied or clean.

#### Scenario: Idempotent teardown
- **WHEN** terminate/remove runs against an already-removed unit
- **THEN** the isolator reports success with no error and no residue

#### Scenario: Capabilities gate contributions
- **WHEN** a contribution requires a capability the active isolator did not declare
- **THEN** the framework refuses at merge time with a typed error naming the missing capability

#### Scenario: Uncertain outcome reconciles
- **WHEN** a mutating operation times out with unknown applied state
- **THEN** the framework re-inspects through the isolator, reports actual state, and either completes or cleans up — never leaves unknown state unreported

### Requirement: External operation wire schemas

Every external operation SHALL define its request/result wire schema as a concrete JSON envelope over the framed protocol. `op` spellings: `isolator.create`, `isolator.initiate`, `isolator.execute_launch`, `isolator.await_result`, `isolator.inspect`, `isolator.state`, `isolator.terminate`, `isolator.remove`. Handles are opaque strings issued by the framework (`unit_handle`, `execution_handle`); never paths, never guest-chosen. Payload object types with required/optional fields:
- `create {spec: {image, mounts[], env[], labels{}, constraints?}, reconciliation_key} → {unit_handle} | error`
- `initiate {unit_handle, reconciliation_key} → {started_attestation {unit_identity, pidns_proof, ready}} | error`
- `execute_launch {unit_handle, execution_handle, argv[], workdir?, stdio_binding, reconciliation_key} → {execution_handle} | error` (bounded; launch never blocks for completion; `execution_handle` is framework-issued in the request since handles are never guest-chosen; `workdir` carries session launch context)
- `await_result {execution_handle} → {exit_status | signal} | {pending}` (uncapped by design; the launching connection owns the await; cancelling detaches without killing; reconnect re-attaches by handle; results replayable until `remove`)
- `inspect {unit_handle} → {snapshot} | error`
- `state {unit_handle} → {lifecycle: created|initiated|executing|stopped|absent}`
- `terminate {unit_handle, grace_ms, reconciliation_key} → {stopped_attestation} | error`
- `remove {unit_handle, reconciliation_key} → {removed_attestation} | error`
Result envelopes are tagged: `{ok: <payload>} | {error: {code, message (value-free), offending_field?}} | {partial: {applied: [...], pending: [...], handles}}`. Long-lived `await-result` responses frame like any other message; the channel stays open across multiple response frames until the terminal result. A request-correlation ID is transport metadata, not a durable identity (see reconciliation).

#### Scenario: Launch returns awaitable handle
- **WHEN** `execute-launch` succeeds
- **THEN** the result carries an execution handle that a later `await-result` redeems for exit/signal delivery; launch itself never blocks for harness completion

#### Scenario: Partial success typed
- **WHEN** an operation partially applies (e.g. unit created but initiate fails)
- **THEN** the result reports the partial form with the created handle included, so the framework can converge or clean up precisely

### Requirement: Conformance suite with real and fake backends

Conformance SHALL split four ways with explicit applicability rules: common contract tests (lifecycle semantics every true backend satisfies), Podman real-lifecycle tests, fake protocol/fault tests, and backend-specific rules declaring which cases apply where. The deterministic fake is classified as a protocol peer and fault injector, explicitly excluded from lifecycle-common tests: the common subset it implements is framing/negotiation refusal and fault-response behavior only, never lifecycle meaning. Podman SHALL prove real lifecycle behavior (create → initiate → execute → inspect/state → terminate → remove, env/mount fidelity, stdio, failure cleanup). The conformance suite pins both `inspect` (read-only snapshot) and `state` (lifecycle enum) plus every state transition operation as distinct scenarios. The fake SHALL prove stdio-boundary behavior the real runtime cannot deterministically produce: malformed frames, unsupported versions/capabilities, oversized output, partial responses, timeout/SIGTERM/SIGKILL/reap, and cleanup ordering under failures. Observable residue and recovery outcomes are defined per case.

#### Scenario: Fake proves boundary refusal
- **WHEN** the fake emits a malformed frame or an unsupported version
- **THEN** the framework refuses deterministically with a typed protocol error

#### Scenario: Podman proves lifecycle fidelity
- **WHEN** the suite conducts a session through the Podman isolator
- **THEN** environment and mounts match the merged plan byte-exact and teardown leaves no residue

#### Scenario: Inspect reads and state transitions are distinct
- **WHEN** the suite exercises `inspect` and `state` alongside state-transition operations
- **THEN** read-only snapshot scenarios, enum-state scenarios, and mutating transition scenarios are pinned separately (a call mixing intents is a distinct defect class)

### Requirement: Explicit descriptor passing for session stdio

Harness stdio SHALL cross to the external guest as explicitly passed file descriptors over a Unix ancillary-data channel (`SCM_RIGHTS`), bound to the specific `execute_launch` request and unit handle — never by inheriting the guest's protocol pipes and never by pathname. Framed protocol stdin/stdout SHALL carry control traffic only. The framework SHALL retain the original FDs until guest acknowledgement; the guest SHALL validate descriptor roles/types, refuse missing/extra FDs, and refuse any fallback to protocol `Inherit` or pathname on FD failure. PTY and piped modes SHALL each preserve their observed `isatty`/HUP/EOF/exit behavior (including piped-conduct deafness); receipt alone never proves correctness. Rebinding on reconnect/retry SHALL be explicit. This supersedes `SessionPty` path-passing as the production mechanism (path validation remains defense-in-depth where paths appear, but no production launch rides a pathname). A future non-Unix isolator SHALL declare its own stdio transport capability; 0.2 Podman is Linux-only, and portability never weakens this contract.

#### Scenario: Piped conduct externalizes unchanged
- **WHEN** conduct runs with piped stdio through the external guest
- **THEN** the harness observes the same deaf/TTY semantics and cleanup as the in-process path, with stdio on passed pipe descriptors

#### Scenario: Wrong FD bundle refuses
- **WHEN** the ancillary channel delivers missing, extra, or role-mismatched descriptors for a launch
- **THEN** the guest refuses with a typed error before spawn; no fallback launch occurs

#### Scenario: Harness bytes never enter the frame parser
- **WHEN** a launched harness writes arbitrary output and reads stdin over PTY and piped transports
- **THEN** no harness byte reaches the framed protocol parser in either mode

### Requirement: Handle eviction and replay binding on the wire

Framework handles SHALL bind to the key and request identity that created them: identical replays succeed idempotently without re-executing, while a known handle with a divergent key or request refuses as mismatched reuse instead of overwriting (and orphaning) live state. Successful `remove` SHALL evict the unit binding and its executions; failed removals keep their handles for retry. Disconnect-time convergence SHALL sweep only units without live executions — live harness processes are left running for the framework's typed teardown path, never swept by the guest.

#### Scenario: Retry does not double-launch
- **WHEN** an `execute_launch` attempt repeats with the identical handle, key, and argv
- **THEN** the guest returns the existing binding without spawning a second harness

#### Scenario: Remove evicts, failure retains
- **WHEN** `remove` succeeds against a bound unit
- **THEN** the binding and its executions clear; a failed remove keeps them for retry

### Requirement: External guest production path

Production `conduct` SHALL drive lifecycle through the external Podman guest binary over the stdio wire; the in-process isolator implementation SHALL remain as the conformance reference and fast-suite backend. Both paths SHALL share wire-schema types by construction, and any behavioral divergence SHALL surface as a conformance failure.

#### Scenario: Wire path carries production traffic
- **WHEN** conduct runs a session with the external guest present
- **THEN** every lifecycle operation crosses the framed protocol and the session converges identically to the reference path

#### Scenario: Divergence is conformance failure
- **WHEN** the wire guest and the in-process reference disagree on any lifecycle behavior
- **THEN** the conformance suite fails naming the divergent operation; the disagreement is never resolved by convention

### Requirement: Removal tombstones reconcile repeatability with eviction

Successful `remove` SHALL evict the live unit binding and its executions on both backends, and record a minimal removal tombstone keyed by the original handle: identical `terminate`/`remove` retries on the tombstone converge residue-free through idempotent backend cleanup (never resurrection), divergent attempts (`initiate`, `execute_launch`, and same-handle `create` on a removed handle) refuse typed, and `state` reports `Absent` / `inspect` reports an absent snapshot. The tombstone retains session identity (container name, session id, local handle for backend delegation) so `converge_clean` on a removed handle still clears surviving scratch. Tombstones are process-scoped (guest lifetime / backend lifetime); no explicit tombstone eviction exists in 0.2. This composes the base repeatable-teardown rule with the base eviction rule: repeatability scopes to tombstones, eviction to live bindings.

#### Scenario: Repeatable teardown on an evicted handle
- **WHEN** terminate/remove runs against an already-removed handle
- **THEN** both backends report success with absent attestations; no unit or execution is resurrected

#### Scenario: Removed handles refuse live operations
- **WHEN** initiate or execute_launch names a removed handle
- **THEN** both backends refuse with a typed error; nothing spawns or starts

#### Scenario: Absent-with-scratch converges by handle
- **WHEN** scratch survives after remove and converge_clean names the removed handle
- **THEN** both backends clear the scratch and report clean

### Requirement: Replay scope after remove

The base replay rule (identical replays succeed idempotently without re-executing) scopes to LIVE handles only: after a successful `remove`, ALL `create` with that handle SHALL refuse typed (`unit handle removed`), including byte-identical replays. A same-handle replay past remove would resurrect the unit and rerun its side effects; fresh units arrive on fresh handles (adopt-or-create keys those), so no legitimate flow re-presents a removed handle. Tombstones intentionally retain no key/spec: there is nothing to distinguish, refusal is uniform. This narrows the base replay promise on removed handles; the merged statement lives here with operator acceptance.

#### Scenario: Replay past remove refuses
- **WHEN** `create` re-presents a handle whose unit was removed
- **THEN** conduct refuses with a typed `unit handle removed` error; no unit is resurrected and no side effects rerun
