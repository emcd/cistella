## ADDED Requirements

### Requirement: Acceptance list declaration

The driver SHALL accept a top-level `environment-acceptances` profile key listing exact invoker-environment variable names to forward into container env. Each name SHALL validate against the env-name grammar; duplicate names SHALL be typed errors reported deterministically (first duplicate in profile-list order). No blacklist, allowlist, or credential-deny SHALL apply to acceptance names: an operator naming a variable — including a secret-shaped one — does so deliberately.

#### Scenario: Valid acceptance list parses

- **WHEN** a profile sets `environment-acceptances = ['AGENTMUX_BUNDLE']` with a well-formed name
- **THEN** resolution accepts the declaration and proceeds to snapshotting

#### Scenario: Duplicate acceptance names fail closed

- **WHEN** a profile lists the same name twice in `environment-acceptances`
- **THEN** conduct fails with a typed error naming the duplicate before any session/runtime mutation

#### Scenario: Ill-formed acceptance names fail closed

- **WHEN** a profile lists a name violating the env-name grammar
- **THEN** conduct fails with a typed error naming the offense before any session/runtime mutation

#### Scenario: Secret-shaped acceptance names are permitted

- **WHEN** a profile lists a credential-shaped name (e.g. a `*_TOKEN` name) in `environment-acceptances`
- **THEN** the declaration is accepted with no deny check; forwarding proceeds under the requiredness and verbatim rules below

### Requirement: Required acceptances with fail-before-session/runtime-mutation

Every listed acceptance is required. Conduct SHALL snapshot all accepted values from the invoker environment before any session/runtime mutation, in profile-list order with deterministic first-error behavior. Any name absent from the invoker environment SHALL fail conduct with a name-only (value-free) diagnostic before unit, scratch, or container creation. XDG profile seeding during named lookup precedes profile parsing and is outside this boundary.

#### Scenario: Absent acceptance fails pre-mutation

- **WHEN** a listed acceptance name is absent from the invoker environment
- **THEN** conduct fails with a diagnostic naming only the variable, leaving no unit file, scratch directory, or container residue

#### Scenario: First absent name in list order reported

- **WHEN** multiple listed acceptances are absent
- **THEN** the diagnostic names the first absent name in profile-list order

#### Scenario: All present acceptances proceed

- **WHEN** every listed acceptance name is present in the invoker environment
- **THEN** snapshotting completes and conduct proceeds to collision checks and rendering

### Requirement: Destination-name collision errors

An accepted name colliding with an `[environment-assignments]` key or with a driver-owned/implicit container name SHALL be a conduct-time error: `HOME` always, plus every name the active credential-surface injects (e.g. `SSH_AUTH_SOCK` when supplied). No silent shadowing occurs in either direction.

#### Scenario: Acceptance overlapping an assignment fails

- **WHEN** a profile both assigns and accepts the same variable name
- **THEN** conduct fails with a typed collision error before any session/runtime mutation

#### Scenario: Acceptance of HOME fails

- **WHEN** a profile lists `HOME` in `environment-acceptances`
- **THEN** conduct fails with a typed collision error (`HOME` is driver-derived from `container-home`)

#### Scenario: Acceptance of an injected credential-surface name fails

- **WHEN** a profile lists a name the active credential-surface injects into the container (e.g. `SSH_AUTH_SOCK`)
- **THEN** conduct fails with a typed collision error before any session/runtime mutation

#### Scenario: Future credential-surface additions extend the implicit set

- **WHEN** a future credential-surface variant injects a new name into the unit's `Environment=` lines
- **THEN** that name joins the implicit collision set as part of the credential-surface change, and accepting it fails like `SSH_AUTH_SOCK` today

### Requirement: Verbatim values through the safety gate

Accepted values SHALL NOT undergo template parsing or rescan: they enter the env map post-substitution and render through the shared `Environment=` unit path. Before rendering, the shared environment-value safety gate SHALL reject non-Unicode and line-breaking host values with value-free diagnostics; ordinary `=` within values is permitted.

#### Scenario: Template-looking values pass through literally

- **WHEN** an accepted host value contains `{{...}}`-shaped text
- **THEN** the value reaches container env byte-identical with no substitution pass

#### Scenario: Line-breaking values rejected value-free

- **WHEN** an accepted host value contains CR or LF characters
- **THEN** conduct fails with a diagnostic that carries no part of the value, before any session/runtime mutation

#### Scenario: Equals signs permitted in values

- **WHEN** an accepted host value contains ordinary `=` characters
- **THEN** forwarding proceeds and the value renders intact

#### Scenario: Accepted values render as unit environment

- **WHEN** snapshotting and collision checks pass
- **THEN** accepted pairs render as `Environment=` unit lines identically to assignments, torn down with the session

### Requirement: Acceptance grants no render permission

Acceptance is conveyance only. Listing a name in `environment-acceptances` SHALL NOT make `{{environment:<name>}}` spans resolve elsewhere in the profile: render permission remains solely governed by the template allowlist. A future renderable opt-in is reserved and explicitly out of scope.

#### Scenario: Accepted-but-unallowlisted spans fail closed

- **WHEN** a profile accepts `FOO` and also references `{{environment:FOO}}` in another field
- **THEN** resolution fails with the allowlist-absent diagnostic; the acceptance confers no render rights and no literal span text reaches the container
