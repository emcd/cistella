## ADDED Requirements

### Requirement: External guest production path

Production `conduct` SHALL drive lifecycle through the external Podman guest binary over the stdio wire; the in-process isolator implementation SHALL remain as the conformance reference and fast-suite backend. Both paths SHALL share wire-schema types by construction, and any behavioral divergence SHALL surface as a conformance failure.

#### Scenario: Wire path carries production traffic
- **WHEN** conduct runs a session with the external guest present
- **THEN** every lifecycle operation crosses the framed protocol and the session converges identically to the reference path

#### Scenario: Divergence is conformance failure
- **WHEN** the wire guest and the in-process reference disagree on any lifecycle behavior
- **THEN** the conformance suite fails naming the divergent operation; the disagreement is never resolved by convention

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

### MODIFIED Requirement: Replay scope after remove

The base replay rule (identical replays succeed idempotently without re-executing) scopes to LIVE handles only: after a successful `remove`, ALL `create` with that handle refuses typed (`unit handle removed`), including byte-identical replays. A same-handle replay past remove would resurrect the unit and rerun its side effects; fresh units arrive on fresh handles (adopt-or-create keys those), so no legitimate flow re-presents a removed handle. Tombstones intentionally retain no key/spec: there is nothing to distinguish, refusal is uniform. This narrows the base replay promise on removed handles; the merged statement lives here with operator acceptance.
