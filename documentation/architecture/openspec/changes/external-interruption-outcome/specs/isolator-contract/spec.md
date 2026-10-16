## ADDED Requirements

### Requirement: Externally-ambiguous await outcomes report observations, never causes

The backend SHALL report a typed `Unobservable` outcome when the
foreground exec client completes nonzero AND independent read-only
lifecycle corroboration finds the container absent at query time
(exit 1), OR finds the service not active (stop/interruption in
flight — mere container existence covers stopped-but-present
containers, so existence alone never gates). The report SHALL carry
the observation record (container-exists code, service states, unit
presence, query ordering) and the raw foreground-client result
(exit status only — no product stderr tail exists on any path),
and SHALL declare the harness outcome unobservable. It SHALL NEVER
claim proven external cause: a normal harness exit followed by
post-completion removal yields the same report truthfully, and
later cleanup confirmation SHALL NOT upgrade the causal
hypothesis. The guarantee applies if AND only if the
`await-interrupted-v1` capability is negotiated both ways;
otherwise legacy mapping holds (documented current behavior), and
an unsolicited variant without negotiation refuses. The guest
advertises the capability ONLY when the host offered it (an old
host admits exactly the offered set). Client failure
with the container present AND the service active, and any
unobtainable facts, SHALL keep ordinary outcome mapping.
Ordinary harness `Exited(137)`, `Exited(143)`, `Exited(255)` with no
external teardown SHALL retain exact current dispositions. A bare
allow-list of raw statuses SHALL NOT satisfy this requirement.

#### Scenario: Absent-at-query reports observably
- **WHEN** the foreground exec client fails and corroboration
  finds the container absent at query time while the harness
  outcome is unobserved
- **THEN** the backend reports `Unobservable` with the observation
  record and raw client result, claiming co-occurrence only

#### Scenario: Inactive service with present container reports observably
- **WHEN** the foreground exec client fails, the container still
  exists, but the service is not active (stop in flight)
- **THEN** the backend reports `Unobservable` with the recorded
  states; mere existence never gates, since stopped-but-present
  containers exist

#### Scenario: Post-completion removal reports without causal upgrade
- **WHEN** the harness exits nonzero normally and another actor
  removes the container before the corroboration query
- **THEN** the backend reports the same observational shape
  truthfully; no proven-interruption claim appears, and later
  cleanup proof does not add one

#### Scenario: Spoofed removal text keeps ordinary semantics
- **WHEN** the harness emits removal-like stderr and exits 255 with
  its container present and no external teardown
- **THEN** the backend reports ordinary `Exited(255)`; no
  unobservable variant appears

#### Scenario: Ordinary nonzero exits unaffected
- **WHEN** the harness exits 137, 143, or 255 ordinarily with no
  external teardown
- **THEN** current dispositions hold exactly; no unobservable
  variant appears

#### Scenario: Schema closure refuses mixed and unsolicited shapes
- **WHEN** an await payload carries mixed top-level tags,
  unknown fields, a typed raw `exited(0)`, or an `unobservable`
  variant without negotiated capability
- **THEN** decoding refuses with a typed contract error; no
  outcome is fabricated from the shape

#### Scenario: Concurrent awaiters replay one frozen outcome
- **WHEN** two awaiters race one completed execution
- **THEN** exactly one corroborates and caches; the other replays
  the frozen outcome identically (bounded, cancel-aware)
