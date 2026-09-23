## ADDED Requirements

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
- `execute_launch {unit_handle, argv[], stdio_binding, reconciliation_key} → {execution_handle} | error` (bounded; launch never blocks for completion)
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
