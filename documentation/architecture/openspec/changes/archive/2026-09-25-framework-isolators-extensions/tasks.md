## 1. Contract and trait

- [x] 1.1 Write the lifecycle/capability contract (phases, contribution schemas, merge rules, deadline ownership) reviewed before any extraction.
- [x] 1.2 Extract Podman behind the framework trait against the contract; bind conformance from the first commit. No behavior change; `conduct` surface untouched.

## 2. Protocol and policy

- [x] 2.1 Implement the stdio protocol host (framing, hello/negotiation, request IDs, bounded responses, kill semantics, reverse cleanup).
- [x] 2.2 Implement the prepare transaction with typed env/mount sets, central merge, and policy evaluation (severity × scope lattice, tighten-only, exact-name acknowledgements, value-free diagnostics).
- [x] 2.3 Implement the credential seam (typed handles, constrained opaque-handle invariant) with fake-driven boundary tests.

## 3. Conformance and first extension

- [x] 3.1 Build the conformance harness: Podman backend proving lifecycle fidelity plus deterministic protocol peer proving stdio-boundary behavior.
- [x] 3.2 Landlock spike as first extension: helper placement, exec ancestry, namespace rule preservation, three-way assertions (admitted/`EACCES`/typed pre-execute `Unsupported`), separate seat.
- [x] 3.3 Agentmux-support and SSH as design vectors and conformance fixtures (no migrations).

## 4. Validation and docs

- [x] 4.1 Dogfood-visibility gate: 0.1.x fleet profiles run unchanged or the slice is not done (Owner proves unchanged-seat startup pre-merge; QA sweeps the fleet pre-release).
- [x] 4.2 Validate: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, fast suite plus live tier on host; `openspec validate --all --strict`.
