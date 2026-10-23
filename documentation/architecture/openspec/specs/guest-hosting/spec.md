# guest-hosting Specification

## Purpose
External guest hosting: shipped sibling-relative guests plus seat-registered third-party guests with digest-bound verification. Synced from changes guest-registration, extension-composition, ssh-extension.
## Requirements
### Requirement: Profile-declared guest selection

Conduct SHALL host exactly the isolator and extensions a profile declares (`[isolator]` table, `[[extensions]]` array of tables): the named isolator backend conducts the session, and each named extension answers prepare. Absent tables mean pure Podman behavior (2.x path — no extension guest spawned, no hooks staged; the Podman isolator guest itself is still hosted as in 2.x, and 0.1.x profiles run unchanged except profiles using the deleted `credential-surface` slot, which refuse at parse). Names SHALL resolve first as shipped guests (`podman`, `landlock`, `tmpdir`, `ssh`, sibling-relative discovery as today), then through the seat-scoped guest registry; names with neither a shipped implementation nor a registry entry, and duplicate extension entries, SHALL refuse pre-create with a typed error naming the declaration (registration is opt-in per guest — an unregistered name can never silently plain a meant-hooked session). Registry entries shadowing a shipped name (`landlock`, `ssh`) SHALL refuse at parse. Guest owner identities SHALL be host-unique namespaced instance IDs distinct from display names: shipped guests own `shipped:<selection-name>`, registered guests own `registered:<entry-name>`; registry entry names SHALL NOT contain `:` (registration-time refusal). Any number of declared extensions conducts (each answers prepare independently in declaration order; each raw plan gates against its own negotiated capabilities pre-concat, then the plans concatenate and the single central merge stays atomic under existing validators — collisions across plans refuse deterministically naming all claimants, hook order follows declaration order, lattice evaluation sees each contribution under its own provenance). At most one contributing plan carries guest hooks, and at most one hook request per contributing plan (admission refuses naming all claimants — the wrapper chain is the only true ceiling). Landlock keeps no merge privilege: shipped-vs-registered is resolution-only and merge treats every provenance uniformly.

#### Scenario: Undeclared sessions run plain

- **WHEN** a profile declares no extensions
- **THEN** conduct spawns no extension guest and runs the plain lifecycle (the isolator guest is hosted as usual); no prepare transaction occurs

#### Scenario: Declared Landlock engages hooks

- **WHEN** a profile declares the `landlock` extension
- **THEN** the Landlock guest answers prepare and its hook stages before create, and the session conducts on the hooked path

#### Scenario: Selected-but-empty hooks refuse pre-create

- **WHEN** a profile declares the `landlock` extension but the guest answers zero hooks (malformed, buggy, or replaced guest)
- **THEN** conduct refuses pre-create with a typed error (never a silent plain session); the selection guarantees engagement

#### Scenario: Unknown guest refuses pre-create

- **WHEN** a profile names an isolator or extension with no shipped implementation and no registry entry
- **THEN** conduct refuses pre-create with a typed error naming the declaration; no session starts undeclared-unhooked

#### Scenario: Registered third-party guest resolves

- **WHEN** a profile names a guest with a valid registry entry (verified source, matching role)
- **THEN** conduct hosts it exactly as a shipped guest of that role (prepare for extensions, lifecycle for isolators); verification failures refuse pre-create typed

#### Scenario: Declared tmpdir resolves shipped

- **WHEN** a profile declares the `tmpdir` extension with no registry entry
- **THEN** conduct spawns the sibling `cistella-extension-tmpdir` binary with install-dir trust (no digest verification — same trust as the landlock sibling) and hosts its prepare like any extension

#### Scenario: Declared ssh resolves shipped

- **WHEN** a profile declares the `ssh` extension with invoker `SSH_AUTH_SOCK` naming a live socket
- **THEN** conduct spawns the sibling `cistella-extension-ssh` binary with install-dir trust and merges its prepare in declaration order like any extension

#### Scenario: N extensions compose in declaration order

- **WHEN** a profile declares `landlock` plus two other extensions (shipped or registered), at most one contributing guest hooks and at most one hook request per contribution
- **THEN** all three answer prepare in declaration order, their plans concatenate into one atomic central merge, and the session conducts with all contributions (hooks staged, mounts bound, slices minted)

#### Scenario: Duplicate extension entries refuse

- **WHEN** a profile declares the same extension name twice
- **THEN** conduct refuses pre-create with a typed error naming the declaration; the same guest is never hosted twice

#### Scenario: Shipped instance identities are namespaced

- **WHEN** a registry entry named `cistella-extension-ssh` conducts alongside declared `ssh`
- **THEN** the entry owns `registered:cistella-extension-ssh` and the ssh instance owns `shipped:ssh`; a declared locator for one never authorizes the other, and an entry named `shipped:ssh` refuses at registration

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

### Requirement: Dispatcher linearization with unified post-terminal error

Admitted SHALL mean present in the dispatcher's `pending` table (inserted only after guest-send success on the single dispatcher thread); mpsc-send success alone SHALL NOT constitute admission. The terminal transition SHALL be `fatal_terminal` draining pending with the composed reason (or orderly Shutdown), `stopping` set, and thread exit; "first terminal" SHALL mean the first fatal-exchange terminal — ordinary per-request ok/error replies SHALL NEVER mark dispatcher terminal. Ops issued after the first fatal terminal is received are DETERMINISTICALLY post-terminal and SHALL fail with ONE documented typed post-terminal error (`CistellaError::Protocol`), replacing the incidental pair. Queued-but-unadmitted ops at transition are post-terminal, not a third case. Ops admitted before terminal SHALL receive their genuine terminal. Uncertainty and death dominance keep their contract: failed shutdown proof sets shutdown-uncertain without latching death; the uncertain path never returns clean.

#### Scenario: Admitted op names the shutdown failure

- **WHEN** an op is admitted before dispatcher terminal and the fatal path's shutdown proof fails
- **THEN** the op fails naming the shutdown failure, the client sets shutdown-uncertain without latching death, and teardown converges by name without ever returning clean

#### Scenario: Post-terminal op gets the unified error

- **WHEN** an op is issued after the first fatal-exchange terminal was received (send-refused or orphaned-disconnect alike)
- **THEN** it fails with the single documented post-terminal `Protocol` error, never with an admitted-path terminal

#### Scenario: Ordinary replies never mark terminal

- **WHEN** ops complete with ordinary ok or error replies while the exchange lives
- **THEN** later ops are unaffected; only a fatal-exchange terminal transitions the dispatcher

#### Scenario: Post-terminal error preserves dominance

- **WHEN** a post-terminal error passes through `death_checked` while the uncertain flag is set
- **THEN** it returns the RECORDED shutdown failure via `shutdown_dominant` (uncertainty/residue dominance preserved, not bypassed)

