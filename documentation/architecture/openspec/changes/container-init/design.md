## Context

Each session's Quadlet `.container` unit runs `Exec=sleep infinity` as container PID 1 (`generate_quadlet_unit` in `src/runtime.rs`). Under PID 1 semantics `sleep` never installs a SIGTERM handler, so `systemctl stop` → `podman stop` waits out the full 10 s `StopTimeout` and SIGKILLs. Every `conduct`/`terminate` teardown pays this tax and the journal records a cosmetic 137 `exit-code` failure. Evidence and fix direction are filed at `cistella:issues/1`. Units are generated per session and never mutated in place; the driver owns the unit text outright.

## Goals / Non-Goals

**Goals:**
- Prompt, signal-clean container stop (SIGTERM → sleep exits 143, already in `SuccessExitStatus`).
- Zombie reaping for any strays the harness leaves behind.
- A timing regression that fails if the 10 s hang ever returns.

**Non-Goals:**
- No `StopTimeout` tuning; the default stays (see Decisions).
- No `inspect` exit-code changes; 137 simply stops occurring, and exit codes already pass through untouched.
- No image changes; the fix stays driver-side (production images are PC Setup-owned).

## Decisions

- **Emit `Init=true` in the `[Container]` section, next to `UserNS=keep-id`.** Quadlet translates it to `podman run --init` (tini as PID 1), which forwards SIGTERM to `sleep` and reaps zombies. Alternative considered: wrapping the Exec command in an init binary (`tini sleep infinity`) — rejected because it would require the init binary inside every image, dragging PC Setup-owned image contents into a driver fix. Alternative considered: a `pre-stop` that execs a kill — racy and redundant once PID 1 handles signals.
- **Keep the default `StopTimeout`.** With forwarding working, stop completes in ~1 s; the 10 s default then only binds genuinely wedged processes, where SIGKILL fallback is exactly what we want. Lowering it would buy nothing measurable and would trim grace from the one path that might need it.
- **No `SuccessExitStatus` change.** The clean path already ends in 143 (`sleep` on SIGTERM), which the unit already accepts. 137 disappears rather than being tolerated.
- **Timing regression bounds the stop phase at 5 s.** Old behavior was deterministically 10.0 s+; new behavior ~1 s; 5 s leaves 5× headroom against a loaded host while still catching any return of the hang. The assertion measures stop/teardown only, not full `conduct`, to avoid harness runtime in the bound.

## Risks / Trade-offs

- [Risk] `--init` needs a tini/catatonit binary where podman runs → Mitigation: podman 4.9 depends on it; failure would surface fail-closed at `conduct` start, and the live timing test proves the path on this host.
- [Risk] Timing assertion flakes on a heavily loaded host → Mitigation: 5× headroom plus stop-phase-only measurement; if it ever flakes, the bound (not the fix) gets revisited.
- [Risk] Sessions created before this change keep old units → Mitigation: none needed — units are per-session and short-lived; no migration, and rollback is deleting one emitted line.

## Migration Plan

None required. New `conduct` invocations emit the new unit; in-flight sessions are untouched. Rollback is removing the `Init=true` emission.

## Open Questions

None. The mechanism (`Init=true` → `--init` → tini) is documented Quadlet/podman behavior, already proven by the dogfood journal evidence showing where the 10 s goes.
