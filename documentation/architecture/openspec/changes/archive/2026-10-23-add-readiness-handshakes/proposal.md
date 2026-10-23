## Why

CI run 39053388013 (ubuntu-26.04, both arches) exposes two timer/ordering races that pass on QA and 24.04 but fail under load: `enter` fires on service-active before the container is exec-ready (issues/12), and the wire_death shutdown assertion observes dispatcher-drop instead of the latched shutdown failure (issues/11). No host CLI surface reads the guest execution table today (todos/49), so tests gate on sleeps and argv inference instead of acknowledged readiness — violating the no-wait-and-pray principle. Tier-2 review further demands: stale-generation binding (same-name recreate must not inherit an old attestation); an exact residue truth table (record-gone never dominates live backend residue); anchored state-root discipline that holds in the `/tmp` fallback; foreign scope honesty (record liveness is not exec proof); and acceptance precision (arm-then-recheck event waits; first-terminal means fatal-exchange terminal).

## What Changes

- `describe state` CLI verb (operator-locked verb-noun; proposal text said `state`) reporting commitment plus snapshot where proven, never `executing`; default `inspect` byte-identical.
- Owner-written host-private launch attestation binding session + container + attempt + argv + handle + backend incarnation (pre-capture before launch, post-check after Ok, publish ONLY on equality via atomic temp+linkat with EEXIST-preserving write-once; mismatch/unknown refuses + converges — false-negative-only, no blessed false positives), published via the established anchored state-root discipline.
- Exact residue/selection table: live backend residue dominates record absence (typed residue error); inert attestation-only residue resolves Absent; full-id probe enables postmortem Absent; fuzzy forms keep Selector errors.
- `enter` single-pass check on commitment + incarnation + backend-appropriate admittability; foreign sessions keep the legacy direct-exec path with no readiness claim (readiness guarantee is native-Podman-scoped).
- Dispatcher linearization with unified post-terminal error and preserved `death_checked` dominance; first-terminal defined as fatal-exchange terminal.
- Commit-event-anchored test waits (inotify arm → recheck → validate-after-wake, fail-loud backstop).
- Ride-along: forced reload-failure exercise for `remove_unit_file` (todos/50).

## Capabilities

### New Capabilities

- None standalone: the query surface extends the existing lifecycle spine rather than creating a new capability area.

### Modified Capabilities

- `framework-lifecycle`: `describe state` verb with commitment/snapshot separation, backend-provenance gating (proven native / unknown / unsupported + query scope), and residue table; attestation durability (temp+linkat) + incarnation contract; native-scoped `enter` edge; per-unit `executing` scoping; selection/postmortem semantics.
- `guest-hosting`: dispatcher linearization (admitted vs queued-unadmitted vs post-terminal), unified post-terminal error, `death_checked` dominance, fatal-terminal definition.

## Impact

- Product: one new CLI verb, one 0600 attestation file per session, `enter` gating (native only), executing-scope repair, one unified dispatcher error, two new typed errors (stale-generation, residue).
- Tests: commit-event waits replace sleeps; same-name recreate/restart pin; residue-row pins.
- Upgrade: no format migration; pre-change sessions read unattested across the window (documented).
- Deps: todos/49 is the product prerequisite; todos/50 rides along as test-only.
