## Why

Every session teardown currently pays a ~10 s tax that has nothing to do with cistella logic: the container's PID 1 (`sleep infinity`) ignores SIGTERM under PID 1 default-disposition semantics, so podman's stop path waits out the full `StopTimeout` and falls back to SIGKILL (evidence: `cistella:issues/1`). The SIGKILL exit (137) also pollutes the journal with cosmetic `Failed with result 'exit-code'` noise. Now is the right time because dogfood runs real sessions daily and the fix is a one-line unit change with a timing regression to lock it.

## What Changes

- Generated Quadlet `.container` units gain `Init=true` in the `[Container]` section, so the container runs under tini: SIGTERM is forwarded to the sleep process and zombies are reaped.
- Stop path becomes effectively instant (SIGTERM → sleep exits 143, already covered by the existing `SuccessExitStatus=143`); the SIGKILL fallback and 137 journal noise disappear.
- A live timing regression asserts teardown completes well under the old 10 s hang, so the behavior cannot silently regress.
- `StopTimeout` stays at the podman default: with signal forwarding working, teardown needs no grace beyond the default, and wedged processes still get the SIGKILL fallback. Recorded as a considered non-change.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `runtime`: the generated Quadlet unit contract gains `Init=true`; the teardown observable changes from "~10 s stop ending in SIGKILL/137" to "prompt stop ending in 143".

## Impact

- `src/runtime.rs` `generate_quadlet_unit` (one directive plus doc comment); unit assertion on generated text.
- Live lifecycle coverage gains a bounded stop-timing assertion; no harness, transport, or profile surface changes.
- Existing sessions are unaffected until their next `conduct` (units are generated per session, never mutated in place).
