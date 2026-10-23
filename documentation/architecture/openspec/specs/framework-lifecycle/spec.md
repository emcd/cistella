# framework-lifecycle Specification

## Purpose
Framework vertical slice (0.2.0): synced from change framework-isolators-extensions. Update Purpose after archive.
## Requirements
### Requirement: Plan/Gate/Apply lifecycle spine

The framework SHALL own session lifecycle as an explicitly ordered spine: (1) pure planning — resolve immutable profile/session input, negotiate capabilities, collect extension plans, centrally merge and validate typed contributions, evaluate policy; (2) gated host pre-create apply; (3) isolator create; (4) isolator initiate/start; (5) post-initiate guest probe and preparation (current mountpoint preparation maps here: it runs after Quadlet initiate via guest exec today); (6) guest restriction/wrapper establishment; (7) bounded execute launch; (8) session-lifetime await/result; (9) reverse-order cleanup/teardown. Bounded control-plane exchanges only: the harness lifetime in phase 8 is intentionally unbounded and never capped. Preparation phases SHALL return plans and SHALL NOT mutate host or container state while planning; mutations occur only at their gated apply phases in spine order.

#### Scenario: Planning is side-effect free
- **WHEN** extension plan collection and central merge run
- **THEN** no unit, mount, file, or container mutation exists until the corresponding apply phase acquires its gate

#### Scenario: Mountpoint preparation maps post-initiate
- **WHEN** the framework prepares mountpoints for a session
- **THEN** preparation runs in phase 5 (post-initiate guest probe and preparation), after the isolator reports initiate complete

#### Scenario: Harness lifetime uncapped
- **WHEN** a session executes its harness in phase 8
- **THEN** no framework deadline terminates the await; only teardown (phase 9) ends it

#### Scenario: Teardown reverses application
- **WHEN** teardown runs after a partially or fully applied session
- **THEN** applied contributions are unwound in reverse order with residue-dominated reporting (residue failure dominates the report)

### Requirement: Baseline binding with gate-held revalidation

Every plan SHALL carry a baseline binding covering at minimum the resolved profile/session digest, the extension executable identity/version, and the relevant resolved resource state. After the resource gate is acquired and before apply, the framework SHALL revalidate the applicable assumptions against live state; drift SHALL fail the plan with a typed error before mutation. Holding the same gate for check and apply closes framework-local races only. Externally mutable paths, symlinks, sockets, and executables require stable OS handles, atomic operations, or immediate apply-time checks; executable identity pinning binds to an opened/stable executable object, never a path re-check followed by a racy open.

#### Scenario: Stale plan refused at the gate
- **WHEN** resource state drifts between plan collection and gate acquisition
- **THEN** revalidation fails the plan with a typed error naming the drifted assumption; nothing is applied

#### Scenario: Extension identity pinned across plan and apply
- **WHEN** the extension executable identity/version at apply differs from the baseline binding
- **THEN** the framework refuses with a typed error; "pinned" means identity stability, not merely avoided PATH lookup

### Requirement: Durable reconciliation identity

Every mutating operation (`create`, `initiate`, `execute-launch`, `terminate`, `remove`) SHALL accept a client-supplied reconciliation key before mutation. The key is a durable resource identity, not transport metadata: opaque client-generated string (UUIDv4 recommended, format unconstrained); uniqueness (one key per resource attempt); reuse (retries of the same attempt carry the same key; distinct attempts carry distinct keys); restart/discovery (the framework lists un-converged keys; a replacement peer presents the key to locate the uncertain resource independent of any returned handle, including a `create` whose handle never arrived); expiry (keys die only with confirmed applied-or-clean convergence). A request-correlation ID SHALL NOT serve as a reconciliation key.

#### Scenario: Replacement peer recovers by key
- **WHEN** a mutating peer is killed and reaped mid-operation with unknown applied state, including a `create` that never returned its handle
- **THEN** the replacement peer presents the same reconciliation key, locates the uncertain resource without the original handle, and converges it to applied or clean with the outcome reported

#### Scenario: Retried mutation reuses key
- **WHEN** a timed-out mutation is retried for the same attempt
- **THEN** the retry carries the identical key and converges rather than duplicating the resource

### Requirement: Trust boundary for pinned planners

Pinned host helpers are trusted code and SHALL keep planning pure by contract; their returned data is nevertheless untrusted and centrally validated like any other extension output. Helpers are NOT sandboxed in 0.2.0 scope. A pinned planner running with user authority that mutates host state outside its returned plan violates its contract; the framework cannot prevent this, only refuse to apply plans whose returned data fails validation.

#### Scenario: Planner output validated despite trust
- **WHEN** a pinned, trusted helper returns a plan containing an inadmissible contribution
- **THEN** central validation refuses the plan exactly as for an untrusted guest; trust waives no check

### Requirement: Hook slots with typed contributions

The framework SHALL expose hook slots with fixed schemas: isolator lifecycle calls, one extension `prepare` transaction per declared extension returning separately-typed environment, mount (placement-only), permission, and policy-claim contribution sets plus guest-hook requests and cleanup declarations, policy evaluation, and teardown observation. Capability advertisement declares which contribution types each guest may return; undeclared types are refused. Each raw plan SHALL gate against its own negotiated capabilities pre-concat — the gate covers the full transacted unit (plan plus credentials plus permissions), so no guest launders a contribution type through another guest's advertisement; policy claims are scoped guest-locally (a guest's claims may target only names it contributed in the same transaction; cross-guest targeting refuses typed). Plans concatenate in declaration order; the single central merge stays atomic under existing validators — collisions across plans refuse deterministically naming ALL claimants, same-target rights join deterministically (Read ∪ Write = Full) with every claimant recorded, duplicate extension declarations refuse at resolution, and lattice evaluation sees each contribution under its host-assigned per-name provenance (multimap — no last-writer attribution; a missing provenance entry is a typed internal refusal, never a guessed identity). At most one nonempty guest-hook set containing at most one hook request is admitted, naming all claimants on refusal (the wrapper chain is the only true ceiling). Intent authority: contributed placements derive bindings from intent; driver bindings render from declared modes and extension intent never vetoes them (recorded for audit and retention input). Post-merge the framework SHALL compute one effective-permissions map (canonical target → joined minimum grants with all provenances, PLUS separate enforcement-bearing binding intent: the most restrictive declared binding per target — read-only if ANY claimant declared read-only, including driver ro and file bindings) as the single source of truth for grant computation, alias guards, RO-retention, descendant checks, topology rendering, the preserved safe RO→RW revision (declared-RO-outside-FULL renders read-write with Landlock carrying the declared intent; retention where the union cannot subtract; files verbatim; revision runs only when a hook is applied — plain sessions render declared modes verbatim), the compile phase input snapshot, and supervisor argv composition. Framework validation of compiler output covers closed output grammar, input coverage and identity, snapshot binding, delegation/claimant limits, unchanged authority metadata, and no extra mutation domains — never recreated backend algorithms. Central merge applies existing per-type validation (mount topology, env gates, collision rules) in spine order. The final topology gate SHALL validate the merged set, run the nested-RO preflight, then pre-create nested bind mountpoints scoped to declared cleanup roots (anchored nofollow walk, 0700; other nesting keeps podman behavior). Teardown SHALL reap extension-declared cleanup paths after converged teardown (identity-recorded at admission, root-admitted, FD-traversed; removal failure is litter, never residue — reap runs only once termination and removal have converged, never while a surviving container or harness could use the paths; the reap covers orderly terminate and natural harness completion alike). `gc` SHALL own retryable cleanup of attributable orphaned slices (session id embedded in names; never under an active session, a live driver, a pending spawn marker, any live recorded guest process group, or an unreadable lease; dead-driver/all-recorded-groups-dead leases are stale) with a durable record and crash/lost-response handling. `gc` SHALL enumerate only the session-attributable prefix pattern, never arbitrary declared names. This spec owns the policy interface (severity × scope, lattice semantics, tighten-only doctrine); claim expression over stdio is owned by the extension-protocol spec. Hook phase dispatch (prepare many, inject-supervisor at most one, finalize many observing) is owned by the extension-hooks spec.

#### Scenario: Undeclared contribution refused
- **WHEN** an extension returns a contribution type absent from its advertised capabilities
- **THEN** the framework refuses the whole prepare transaction with a typed error before any mutation

#### Scenario: Cross-contribution planning is atomic
- **WHEN** one prepare transaction returns both environment and mount sets
- **THEN** central validation sees both sets together and either accepts the merged plan or refuses it whole; partial application never occurs

#### Scenario: Contributed host state reaped
- **WHEN** a session conducted with an extension that declared cleanup paths ends with converged teardown
- **THEN** the declared directories are removed; a removal failure reports litter without failing the teardown verdict; undeclared contributed mounts survive

#### Scenario: Reap waits for convergence
- **WHEN** termination or removal has not converged (a container or harness may survive)
- **THEN** no reap runs; the paths remain until convergence or `gc`

#### Scenario: gc owns orphaned slices
- **WHEN** slices exist for sessions with no live unit (including prepare-failure orphans with no merged plan)
- **THEN** `gc` removes them retryably; slices under active sessions are never touched

#### Scenario: Nested mountpoints pre-created under cleanup roots
- **WHEN** the merged topology nests a bind under another bind whose host source sits at or under a declared cleanup root
- **THEN** the framework pre-creates the mountpoint seat-owned before container start through an anchored nofollow walk (planted symlinks refuse); nesting elsewhere keeps podman behavior

#### Scenario: N plans merge in declaration order
- **WHEN** a profile declares three extensions that each answer prepare validly
- **THEN** each raw plan gates against its own negotiated capabilities, the plans concatenate in declaration order, and one atomic central merge admits them with per-name provenance

#### Scenario: Collision names all claimants
- **WHEN** two extensions contribute the same environment name with different values
- **THEN** the merge refuses with a typed error naming both claimants; no partial plan applies

#### Scenario: Same-target rights join with all provenances
- **WHEN** two extensions declare rights on the same canonical container target (`Read` and `Full`)
- **THEN** the effective permission joins to `Full` deterministically with both claimants recorded; no refusal, no silent widening beyond the join

#### Scenario: Credential laundering refuses pre-concat
- **WHEN** GuestA returns credentials it never advertised while GuestB advertises the credentials type
- **THEN** GuestA's own per-plan gate refuses before concatenation; the later union gate never sees the laundered unit

#### Scenario: Cross-guest claim targeting refuses
- **WHEN** GuestA's policy claim targets a name only GuestB contributed in the same session
- **THEN** GuestA's guest-local scope check refuses typed; claims never evaluate against another guest's names

#### Scenario: Missing provenance refuses internal
- **WHEN** a merged name carries no recorded contributor (merge bug, never operator input)
- **THEN** evaluation refuses with a typed internal error; no fallback identity is guessed

### Requirement: Suppressible universal policy cell

A `suppressible × universal` denial SHALL refuse conduct for any provenance unless an exact-name acknowledgement exists; with a matching acknowledgement the variable forwards normally. Diagnostics SHALL report name, scope, and severity without values.

#### Scenario: Unacknowledged suppressible refuses
- **WHEN** a variable matches a `suppressible × universal` denial and no exact-name acknowledgement exists
- **THEN** conduct fails with a typed policy error naming the variable, its scope, and its severity — never its value

#### Scenario: Acknowledged suppressible forwards
- **WHEN** the same match carries an exact-name acknowledgement in user policy
- **THEN** forwarding proceeds; the acknowledgement is recorded in the diagnostic trail

### Requirement: Suppressible on-extensions policy cell

A `suppressible × on-extensions` denial SHALL apply only to extension-contributed values, with the same refuse-unless-acknowledged behavior and value-free diagnostics. Profile-supplied values never match this cell.

#### Scenario: Extension contribution matches, profile value does not
- **WHEN** an extension contributes a variable matching a `suppressible × on-extensions` denial while the profile assigns the same name cleanly
- **THEN** the contributed value refuses unless acknowledged; the assigned value is unaffected

### Requirement: Inviolable universal policy cell

An `inviolable × universal` denial SHALL refuse conduct for any provenance with no acknowledgement path. Only the site principal establishes this cell; user policy and profiles SHALL NOT create, weaken, or acknowledge around it.

#### Scenario: Inviolable refuses despite acknowledgement
- **WHEN** a variable matches an `inviolable × universal` denial even with an exact-name acknowledgement present
- **THEN** conduct fails with a typed policy error; the acknowledgement is inert

### Requirement: Inviolable on-extensions policy cell

An `inviolable × on-extensions` denial SHALL refuse matching extension contributions with no acknowledgement path; neither profiles nor extensions may override it. Site authority establishes the floor; user policy may add rules in this cell but SHALL NOT weaken a site floor.

#### Scenario: Site floor survives user policy
- **WHEN** user policy omits or contradicts a site-established `inviolable × on-extensions` denial
- **THEN** the site floor enforces; user policy cannot narrow scope or lower severity

### Requirement: Acknowledgement authority and precedence

Exact-name acknowledgements SHALL live in user `policies.toml` only — never in profiles, never as a conduct-time flag in the first policy version. Precedence is site > user > per-profile declarations > compiled defaults, tighten-only downward: a finer layer SHALL add constraints, widen scope, or raise severity, and SHALL NOT remove, narrow, or lower. Compiled defaults are suppressible only. The root-owned site source (`profiles/13`) is seam-only in 0.2.0: no compiled default masquerades as site authority. Extension `policy_claims` are scoped to their own transaction and contributions; an extension SHALL NOT rewrite persistent or global policy. Grandfathering precedence: a shipped 0.1.1 `environment-acceptances` entry is grandfathered against compiled-default rules only — an exact acceptance is not itself an acknowledgement, user/site suppressible denials still require explicit acknowledgement, and later rules apply normally. No permanent exception defeats universal policy.

#### Scenario: Defaults never inviolable
- **WHEN** a denial exists only as a compiled default
- **THEN** it behaves suppressible; nothing in defaults refuses unconditionally

#### Scenario: Claims cannot persist policy
- **WHEN** an extension returns a `policy_claim` reaching beyond its transaction
- **THEN** central validation refuses the whole transaction with a typed error; overreach is never stripped-and-continued (atomicity holds)

#### Scenario: Shipped acceptances grandfathered against defaults only
- **WHEN** a 0.1.1 `environment-acceptances` entry forwards under its original rules
- **THEN** no new acknowledgement is required against compiled-default rules, but user/site suppressible denials still require explicit acknowledgement and later rules apply normally

### Requirement: User policies.toml file contract

User policy SHALL live at `$XDG_CONFIG_HOME/cistella/policies.toml`, falling back to `~/.config/cistella/policies.toml`. An absent file means compiled defaults only. The file carries `format_version = 1`; unknown fields, duplicate entries, and malformed TOML refuse conduct fail-closed before planning. Rules are `[[denials]]` entries `{pattern (regex, compiled once at load), severity (suppressible|inviolable), scope (universal|on-extensions)}`; acknowledgements are `[[acknowledgements]]` entries `{name (exact, env-name grammar)}`. A user entry combining `severity = inviolable` with `scope = universal` refuses at parse time with a typed error: that cell is site-authority only and admitting it would let user policy impersonate site policy. The file content hash joins the plan baseline binding. Format upgrades refuse unknown versions with guidance; rollback is file restore (no migration machinery in 0.2.0).

#### Scenario: Absent file means defaults
- **WHEN** no user `policies.toml` exists
- **THEN** compiled-default (suppressible-only) rules apply and conduct proceeds normally

#### Scenario: Malformed file refuses pre-planning
- **WHEN** the file has unknown fields, duplicates, bad regexes, or unknown versions
- **THEN** conduct fails with a typed policy error before any planning; nothing is evaluated against a half-read file

### Requirement: Framework-owned deadlines

The framework SHALL bound every control-plane guest interaction (the harness lifetime itself is uncapped per the spine): short hello, bounded plan/apply windows, SIGTERM grace then SIGKILL, capped frame/message sizes and stderr, typed timeouts, fail-closed before execute. Guests SHALL NOT choose unbounded timeouts. The framework SHALL drain stdout and stderr concurrently and boundedly; SHALL own, terminate, and reap its process group and verify no descendant retains protocol FDs (pipe-EOF proof); SHALL reconcile and clean up after a timed-out mutating operation with uncertain outcome; and SHALL report with residue dominating (residue failure outranks all other outcomes). Mechanism (process groups, pidfds) and numeric values are implementation choices. Supervision assumes sane, non-malicious helpers (trusted code, untrusted data per the trust boundary): a descendant that deliberately escapes its process group AND closes every protocol FD is outside enforcement — hunting escapees by PID risks killing an innocent process after PID reuse, so the framework does not try. That residual is documented and pinned by conformance, never silently absorbed. Long-lived concurrent observers are out of scope; the event-stream capability is reserved but unbuilt.

#### Scenario: Slow guest fails closed
- **WHEN** an extension exceeds its plan window
- **THEN** the framework kills it, reaps it, and fails conduct before execute with a typed timeout error and no residue

#### Scenario: Descendant holders reaped
- **WHEN** a guest forks descendants that retain protocol FDs past SIGTERM grace
- **THEN** the framework terminates and reaps the whole group and verifies pipe EOF; no FD-holder survives the window

#### Scenario: Group-escapee residual documented
- **WHEN** a descendant escapes its process group and closes every protocol FD
- **THEN** shutdown may report clean while the escapee survives; this residual is outside enforcement (PID-hunting risks innocents) and stays pinned by conformance, never absorbed silently

#### Scenario: Uncertain mutation reconciled
- **WHEN** a guest times out after creating a resource but before returning its handle
- **THEN** the framework reconciles actual state, cleans up or completes, and reports residue first

### Requirement: Live guest prepare and hook phases

The spine's prepare and guest-hook phases SHALL carry live traffic with real external guests: `prepare` returns actual env/mount contributions plus hook requests, the guest-context capability probe runs after initiate, apply confirms before execute, and wrapper diagnostics flow on the dedicated channel. Framework deadlines SHALL bound every exchange; timeouts fail pre-execute with no residue.

#### Scenario: Live prepare merges real contributions
- **WHEN** the Landlock extension returns its prepare transaction
- **THEN** central merge validates and admits the typed sets exactly as with the fixture peer, and the merged plan drives the session

#### Scenario: Probe failure fails live pre-exec
- **WHEN** the real guest's capability probe reports a required restriction unsupported
- **THEN** apply never runs, staged artifacts are removed, and conduct fails with a typed capability error

### Requirement: Conduct finalizes unobservable outcomes after teardown proof

`conduct` SHALL map a backend `Unobservable` report through teardown and report the finalized typed outcome as a driver-level report (typed one-line stderr rendering, exit 1 — the existing class for paths that cannot report a harness disposition), never as a harness disposition (never exit 128+N, never exit 0). Proof dominance SHALL hold in phase order — teardown, then guest release (quiescence), then fresh backend proof, then render: any teardown error dominates, then release failure, then backend absence re-proof — observation errors and unconverged proofs dominate with their own reports, and `cleanup=converged` prints only on the backend's converged bit. The absence proof SHALL re-query container, service, and unit with checked queries (manager failure, unexpected shapes, and non-NotFound metadata errors are observation errors); convergence requires container-exists exit 1 AND an explicit terminal service state (`LoadState` `not-found` or `ActiveState` `inactive`) AND absent unit file — every other service shape fails convergence, and a stopped-but-present container never converges. Proof attests native backends only: registered (foreign) guests refuse explicitly, since local queries prove nothing about their resources. A preexisting hooked exec/transition failure SHALL keep its disposition precedence over the unobservable arm. The rendering SHALL name the observation (container fact from the recorded code, never fixed), state that the harness outcome is unobservable, render the raw client result as `exited(code)` or `signaled(n)` without coercion, and carry no product stderr tail (none exists). Final absence facts (unit file, explicit service state with checked query, container-exists exit 1) SHALL hold exactly as in the converged path.

#### Scenario: Unobservable conduct reports typed, exits 1
- **WHEN** conduct's await observes container-absent-at-query and teardown converges with absence proven
- **THEN** conduct prints the typed observation report and exits 1; absence facts hold; no harness signal disposition and no proven cause are claimed

#### Scenario: Residue dominates the observation report
- **WHEN** teardown after an observed interruption leaves residue or ends uncertain
- **THEN** the residue/uncertain outcome reports instead; no `cleanup converged` prints

#### Scenario: Boundary asserts observation set per interleave
- **WHEN** the live CLI-vs-attached boundary observes an attributed interleave
- **THEN** it asserts the observation record plus explicit absence facts plus retained raw (test taps), pinned per interleave; exact raw dispositions remain pinned at unit level

### Requirement: Dedicated state query verb

The host CLI SHALL provide `describe state` reporting launch commitment plus a backend snapshot. The verb SHALL resolve ownerlessly and report: a commitment bit (attested with attempt provenance, or unattested) and — only where backend provenance exists — a snapshot enum from created/initiated/stopped/absent in verbatim lowercase. The verb SHALL NEVER report `executing` (not ownerlessly provable; reserved to the in-protocol scoped meaning). Attested+running SHALL read `initiated`. Backend provenance rule: an attestation carrying an incarnation proves the publisher was native (backend fact, stable); live incarnation equality proves currency — the same incarnation still runs, checked per read on the enter edge. Provenance without currency still renders the snapshot with its incarnation qualifier (`current`/`stale`/`unknown`), because commitment and currency are independent facts and the qualifier names which holds. Unincarnated commitment with no record proves nothing locally (unsupported); a record with no attestation proves nothing about backend (explicitly `backend=unknown` with NO snapshot — the upgrade-window cost). Accepted limit, recorded not waived: a pre-change foreign session (no record, no attestation, no container) is indistinguishable from a typo'd id under a native-scoped probe, and resolves `absent` exactly like one — not global-absence knowledge, only the probe's local result. Selection: no-match in any form keeps the existing typed Selector error EXCEPT a FULL session id matching no record, which probes the derived container name and follows the residue table with query scope marked (`scope=native`, never a global `backend=` claim). Podman-specific facts SHALL be gated to proven-native sessions. Record + unincarnated commitment (contradictory provenance — foreign conducts create no unit files) SHALL refuse typed, never render. Default `inspect` SHALL remain byte-identical.

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

