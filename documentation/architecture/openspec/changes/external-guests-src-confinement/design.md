## Context

The framework half (archived `2026-09-25-framework-isolators-extensions`) delivered contracts, the stdio host, prepare/policy/seam, and fixture-only guests. `conduct` still drives the in-process `PodmanIsolator` directly; no guest binary is spawned in production and no prepare transaction carries live traffic. Fleet seats run 0.1.x with `~/src` mounted broadly RW and no confinement. Owner seat is containerized (no podman/systemd); QA tandem seat carries all live proof. Operator blocks 0.2.0 fleet deploy until real isolator + extension architectures are demonstrated with `~/src` safe.

## Goals / Non-Goals

**Goals:** Podman lifecycle over the stdio wire through a real guest binary; Landlock extension answering real `prepare` plus guest-hook delivery; `~/src` subtree confinement demonstrated with denial evidence; conduct switched to the external path behind the unchanged-seat proof.

**Non-Goals:** additional isolators; separate crates/workspace split; seat-runtime-dir provisioning (seat-socket stays fixture-only per the standing tier-2 disposition); per-profile denials; mid-session observation; fleet profile migration (0.1.x runs unchanged).

## Decisions

- **Same-crate `--bin` guests, not separate crates.** One version, one release, one `cargo install` delivering `cistella` plus guests; discovery via the install sibling directory (current-exe-relative), never PATH-searched, satisfying the pinned-discovery rule with zero new machinery. Rejected: separate crates (version skew between driver and guest, publishing chore, no benefit at this scale).
- **In-process impl stays as conformance reference.** `PodmanIsolator` keeps driving fast unit tests and the lifecycle-fidelity suite; production `conduct` uses the wire guest. The two must agree by construction (shared wire-schema types), and any divergence is a conformance failure, not a judgment call. No flag-gated production fallback exists in 0.2: a missing or crashed guest fails conduct loudly (typed discovery/timeout error) and never silently falls back to in-process execution or skips Landlock. Rejected: keeping the in-process production call as fallback (a silent confinement-skipping path is worse than a loud failure) and deleting the in-process path (loses the fast suite and the only debugger-friendly backend).
- **Confinement rule shape from the session directory.** The project subtree is already canonicalized in `conduct` (`~/src/<project>` or `~/src/CLONES/<project>/<lane>`); conduct translates it through the validated guest mount topology, and the Landlock guest grants R+X on the translated guest ancestor route with full rights on the translated guest subtree route — union semantics make the exception exact with no new profile schema and no per-seat rules to author. Rejected: profile-declared confinement rules (new schema, new audit surface, for a shape the driver already knows).
- **Denial evidence, not intent review.** The gate is attempted writes failing with `EACCES` in live conformance (spike three-way style extended to the subtree), plus the unchanged-seat proof for everything else. A design doc claiming safety is not evidence.
- **Guest binaries ship.** `package.include` must cover the new bins; the fake peer stays excluded. Install footprint grows by two small helpers alongside `cistella`.
- **Trusted install directory assumption (explicit).** Sibling-relative discovery plus opened-FD digest avoids PATH and replacement races, but same-crate shipping does not authenticate a malicious sibling planted before planning. 0.2 assumes the install directory and both binaries are operator-owned and unmodified (standard installed-software trust); both bins must ship together and a guest/version mismatch SHALL fail hello before any mutation. Separate crates are not required under this explicit assumption.

## Risks / Trade-offs

- [Risk] External guest crash mid-lifecycle strands a unit → Mitigation: reconciliation by key already specced and conformance-pinned; pre-exec crashes recover by re-exec of the stateless guest, while death during execute/await converges to clean via typed teardown (no handle resurrection). Full cross-crash execution survival is deferred, not promised.
- [Risk] Landlock unavailable (old kernel, no no_new_privs path) → Mitigation: helper's typed `Unsupported` already proven; conduct fails pre-execute with a capability error, never silently unconfined. Confinement is fail-closed or absent-loudly, never absent-silently.
- [Risk] Extra process hop per lifecycle op → Mitigation: negligible against container-operation latency; bounded by the existing framework deadlines either way.
- [Risk] Conduct switch regresses fleet seats → Mitigation: the dogfood gate repeats (unchanged-seat proof + fleet sweep) against the external path; rollback is revert to the in-process call.

## Migration Plan

Land as additive bins plus a conduct call-site switch; fleet profiles and session state untouched. Rollback is commit revert (in-process path remains compiled and tested throughout).

## Open Questions

- Guest binary names (`cistella-isolator-podman`? `cistella-extension-landlock`?).
- x86_64 loader parity for the shipped Landlock wrapper (spike cross-checked the test wrapper; the shipped artifact needs the same proof).
- Full cross-crash execution survival (framework-side execution ownership outliving the guest) is deferred; recovery stays pre-exec plus typed teardown. Revisit only with a design that keeps handle redemption sound.
