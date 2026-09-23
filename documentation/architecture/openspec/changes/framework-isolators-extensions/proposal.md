## Why

Cistella 0.1.x is a Podman driver with hand-rolled per-profile wiring: relay identity forwarding, socket mounts, and harness plumbing are repeated in every Agentmux-integrated profile, and confinement ideas (Landlock, FUSE views) have nowhere to attach. The 0.2.0 vertical slice reframes Cistella as a framework — lifecycle ownership plus typed contribution contracts — with Podman extracted as the first isolator and Landlock as the first extension. Agentmux-support and SSH serve as design vectors and conformance fixtures, not migrations.

## What Changes

- Lifecycle spine under framework ownership (Plan/Gate/Apply): pure planning → gated host pre-create apply → isolator create → isolator initiate/start → post-initiate guest probe and preparation → guest restriction/wrapper establishment → bounded execute launch → session-lifetime await/result → reverse-order cleanup/teardown. Bounded control-plane exchanges only; the harness lifetime itself is intentionally unbounded and never capped. Preparation returns plans, never mutates while planning; current mountpoint preparation maps to the post-initiate phase (it runs after Quadlet initiate via guest exec today).
- Podman extracted behind a narrow transport/isolator trait **after** the lifecycle/capability contract is written (never letting current call sites define the abstraction); bound immediately to conformance tests.
- One extension `prepare` transaction returning separately-typed environment and mount contribution sets (distinct schemas, framework-owned ordering and central merge with existing collision rules); capability advertisement declares returnable contribution types.
- Host preparers distinguished from guest pre-exec extensions (Landlock-style restrictions inherit through descendants and cannot bolt onto a running harness; the spike proves helper placement and exec ancestry).
- Versioned stdio JSON protocol for external isolators/extensions (length-prefixed framing, request IDs, negotiation, capability sets, max frame, deterministic unknown-field/version refusal, stderr-only logs, executable discovery/pinning); all extension output revalidated centrally as untrusted input.
- Policy evaluation as an interface input: severity (`suppressible`/`inviolable`) × scope (`universal`/`on-extensions`), tighten-only downward across site > user > defaults; exact-name acknowledgements in user `policies.toml` (no conduct flag, never profiles); invalid policy claims refuse the whole prepare transaction (weak-but-valid claims resolve by tighten-only precedence); shipped 0.1.1 acceptances grandfathered against compiled defaults only, with user/site rules applying normally; value-free diagnostics. `assert_no_github_token` stays `suppressible × universal`.
- Credential seam as typed contribution/capability with a constrained opaque-handle invariant (framework-issued handles only).
- Deterministic protocol peer (test-double, excluded from lifecycle-common proof) from the first protocol series for stdio-boundary validation (malformed frames, version refusal, oversize, timeout/kill/reap, cleanup ordering); Podman proves real lifecycle.
- **BREAKING** (pre-1.0, as needed): internal module layout (`isolators/podman`); no profile-schema break is planned.
- Explicitly out: mid-session observation API (event stream reserved, unbuilt); per-profile denials; QoL riders (pure theme; possible Trixie image chore pre-release); Agentmux/SSH migrations (vectors only).

## Capabilities

### New Capabilities

- `framework-lifecycle`: spine phases, plans-not-mutations, gates, hook slots, deadline ownership, reverse cleanup, policy evaluation inputs.
- `isolator-contract`: isolator interface (`create`/`initiate`/`execute`/`inspect`/`state`/`terminate`/`remove`), declared capabilities, ownership and idempotent-teardown semantics, conformance suite (Podman lifecycle proof + deterministic protocol peer for boundary faults).
- `extension-protocol`: stdio transport, framing, negotiation, prepare transaction with typed contribution sets, policy claims, guest-hook requests, trust boundaries.

### Modified Capabilities

- `transport`: Podman call sites extracted behind the framework trait; behavior unchanged, contract-bound.

## Impact

- `src/` reorganization (`isolators/podman`, framework core, extension host); new protocol and policy modules; conformance harness (Owner: interface/spec + extraction; QA: conformance + Landlock evidence).
- Landlock as first extension (separate seat; three-way assertions: admitted / `EACCES`-denied / typed pre-execute `Unsupported`).
- No fleet profile rewrites required (framework keeps 0.1.x seats runnable — dogfood-visibility condition).
