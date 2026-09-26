## ADDED Requirements

### Requirement: Pinned guest binary discovery

The framework SHALL discover external guest binaries sibling-relative to the installed driver executable (current-exe directory), never via PATH search. A missing or non-executable guest binary SHALL fail conduct pre-create with a typed discovery error naming the expected binary, never a spawn of an untrusted path. A discovered guest whose protocol version or capability set is unsupported SHALL fail hello negotiation with a typed version or capability error before any planning or mutation.

#### Scenario: Missing guest refuses pre-create
- **WHEN** the expected guest binary is absent from the install sibling directory
- **THEN** conduct fails before unit creation with a typed discovery error; no session state is mutated

#### Scenario: PATH never consulted
- **WHEN** an executable of the guest's name exists on PATH but not beside the driver
- **THEN** discovery ignores it; conduct fails with the same typed discovery error

#### Scenario: Version mismatch fails hello pre-mutation
- **WHEN** a discovered guest binary advertises an unsupported protocol version
- **THEN** hello negotiation fails with a typed version error before any planning or mutation

#### Scenario: Capability mismatch fails hello pre-mutation
- **WHEN** a discovered guest binary advertises an unsupported capability set at a supported protocol version
- **THEN** hello negotiation fails with a typed capability error before any planning or mutation

### Requirement: Wire hosting with framework deadlines

The framework SHALL host each guest over stdio with the existing framed protocol and framework-owned deadlines (hello/plan/apply bounds, SIGTERM grace then SIGKILL, group reap with pipe-EOF proof). A guest that exceeds a bound SHALL be killed, reaped, and failed with a typed timeout error leaving no residue.

#### Scenario: Slow guest killed at the bound
- **WHEN** an external guest exceeds its plan window
- **THEN** the framework kills and reaps its group, verifies pipe EOF, and fails pre-execute with no residue

### Requirement: Stateless guests with key reconciliation

External guest pre-exec operations (create, initiate) SHALL be recoverable by stateless re-exec: every mutating op carries the framework-issued reconciliation key, and pre-exec crashes converge by re-exec plus key-based locate, never by guest-side recovery state. Execute/await remains guest-owned as below. Handles stay framework-issued opaque strings end to end.

Recovery scope is pre-exec only: create and initiate converge by re-exec plus key. A guest death during execute/await SHALL NOT attempt handle resurrection (execution ownership — Child, PTY binding, outcome recording — dies with the guest and must not be reaped from a stranger); the framework SHALL converge the session to clean via typed teardown and refuse further operations on the dead execution. Full cross-crash execution survival is deferred, not promised.

After any abnormal guest exit the framework SHALL check unit residue by reconciliation key over every recorded attempt key (a latched death observation — never error-text matching — gates the check); located residue fails loudly and dominates the report instead of passing silently.

Shutdown proof is part of the death observation: the dispatcher attempts bounded shutdown/reap before failing pending callers. Proven shutdown latches dead-or-reaped (residue check valid); FAILED shutdown sets a separate shutdown-uncertain state WITHOUT latching (the guest or a descendant may live — no keyed scan runs), records the proof failure, and still converges by name. The uncertain path SHALL never return clean: the shutdown residue class dominates even when a post-converge snapshot finds nothing (a survivor could install after the check).

#### Scenario: Crashed guest recovers by re-exec
- **WHEN** a guest dies mid-create or mid-initiate with unknown applied state
- **THEN** the framework re-execs the guest binary, presents the same key, and converges the resource to applied or clean

#### Scenario: Pre-exec recovery replaces once, then fails stop
- **WHEN** the replacement guest also dies during the retried create/initiate
- **THEN** conduct fails stop with the second failure dominant (first retained as context) and converges by name; no further re-exec is attempted

#### Scenario: Ambiguous launch submission never replays
- **WHEN** the guest dies with an `execute_launch` call submitted and no terminal response
- **THEN** the framework treats it as execution-boundary death (spawn precedes reply, so a harness may outlive the guest): typed teardown via the dead-guest path, never a re-launch; the harness is never run twice and no outcome is fabricated

#### Scenario: Death during execute tears down typed
- **WHEN** a guest dies after execute_launch or during await
- **THEN** the framework kills any surviving harness side, converges to clean via typed teardown, and refuses handle redemption with a typed error; no outcome is fabricated
