## ADDED Requirements

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

The framework SHALL expose hook slots with fixed schemas: isolator lifecycle calls, one extension `prepare` transaction returning separately-typed environment and mount contribution sets plus policy claims and guest-hook requests, policy evaluation, and teardown observation. Capability advertisement declares which contribution types each guest may return; undeclared types are refused. Central merge applies existing per-type validation (mount topology, env gates, collision rules) in spine order. This spec owns the policy interface (severity × scope, lattice semantics, tighten-only doctrine); claim expression over stdio is owned by the extension-protocol spec.

#### Scenario: Undeclared contribution refused
- **WHEN** an extension returns a contribution type absent from its advertised capabilities
- **THEN** the framework refuses the whole prepare transaction with a typed error before any mutation

#### Scenario: Cross-contribution planning is atomic
- **WHEN** one prepare transaction returns both environment and mount sets
- **THEN** central validation sees both sets together and either accepts the merged plan or refuses it whole; partial application never occurs

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
