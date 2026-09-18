## Context

Proven live 2026-09-18 (scratch `default`-profile sessions, since torn down): `podman exec` runs as uid 1000; a triple targeting `/home/cistella/.config/deep/nest` yields root-owned `.config` and `.config/deep` with a me-owned `nest`; `mkdir /home/cistella/.config/agentmux` as the seat fails `Permission denied`. No `--user` exists anywhere in driver code or the image — the 1000 default is observed podman behavior, not a contract. The conduct flow (`conduct_session`, `src/main.rs:244-446`) already has the shape this needs: lock-guarded creation window, fail-closed teardown after install/start, harness attach last.

## Goals / Non-Goals

**Goals:**
- Sessions can create sibling dirs under auto-created mount parents, for seat-owned work, without per-dir triples.
- Never alter host-side ownership (mount roots are a hard boundary), never touch `/`, idempotent re-runs.

**Non-Goals:**
- No pinning of the harness exec user (transport follow-up, separate change).
- No changes to tmpfs home handling (arrives 1777, works; document, don't touch).
- No profile schema or CLI surface changes.

## Decisions

- **Prepare after start, before harness attach, inside the creation-window lock.** The container must exist (exec needs it running); the harness must not race preparation. The lock already spans install→start; extending through a bounded exec sequence keeps `gc`/`terminate` from observing a half-prepared session. Alternative considered: host-side `podman unshare mkdir/chown` before start — rejected because host-side ownership is the wrong namespace to reason in (keep-id maps container-1000↔host-1000, but podman-created parents' host ownership is an implementation detail we should not depend on); in-container preparation sees exactly what the seat sees.
- **Read the seat uid live (`podman exec <ctr> id -u`).** Alternative considered: assume 1000 from keep-id — rejected; the whole bug class comes from assuming uid behavior instead of measuring it.
- **Pin `--user root` on setup execs.** The observed exec default (1000) would make mkdir/chown silently ineffective; explicit root keeps setup effective even if podman changes its default. Seat-uid chown target comes from the live read, so the two never drift.
- **Conditional chown (only uid-0 dirs), unconditional mkdir -p, batched execs.** Re-chowning deliberately-owned dirs would fight image content and prior runs; touching only root-owned dirs makes the step a convergent fixpoint. Execs are batched, not per-directory: one `stat` exec over all computed ancestors, then one `mkdir -p` over the full set, then (only if the stat showed root-owned entries) one `chown` over that subset — three bounded execs worst case, typically two. Mountpoint ancestors (all triple targets, tmpfs home, session-home target) and `/` are excluded before any exec — computed purely from the profile + session, unit-tested without podman.
- **Runtime containment proof per candidate; lexical walk stays as candidate generation only.** String exclusion cannot see through symlink components in the running image (`/alias/deep` with `/alias` → `/work` would chown a host-backed mount while equaling no registered target). Each lexical candidate is therefore resolved in-container (symlink components followed) and its containing mount determined; the candidate is authorized only if the resolved path lies beneath no bind-mount target. Tmpfs-home subtrees are explicitly authorized (container-ephemeral). Containment unprovable for any reason → fail closed, no mkdir/chown. Implementation picks the mechanism (`df`/`findmnt` containing-mount query or symlink-safe descriptor walk); the contract is the authorization predicate, unit-tested purely, plus an adversarial symlink-to-bind-mount live case.
- **Authorization excludes everything beneath any bind-mount target, not just equality.** `validate_mounts` permits descendant mounts under read-only ancestors, so stopping at exact mountpoints is too late: for RO `/tree` with a child target below it, `/tree/deep` is inside the RO bind subtree and must be excluded even though it equals no target. The tmpfs-home case is the principled exception (ephemeral, no host backing). Nested-RO exclusion and tmpfs-home eligibility are pinned by unit and live cases.
- **Fail closed through the lock-held teardown form.** Preparation runs under the creation-window lock, so its failures must call the lock-held inner teardown (`teardown_inner`), never the lock-acquiring `teardown` — re-acquiring the held lock would deadlock the very path that guarantees no residue.
- **Setup exec discipline: argument vectors with `--` before paths, one bounded numeric UID parse, explicitly bounded execs.** Paths never transit through shell strings; the live `id -u` read parses as a single bounded integer (anything else is a typed error, not a default); every spawned exec carries an explicit deadline inside the existing bounded-exec discipline.
- **Setup binaries resolve from the selected image while running as container root.** Documented as assumed authority: preparation `stat`/`mkdir`/`chown` are whatever the image ships. If images ever stop being a trusted boundary, this step gains authority over RW mounts and needs re-review — recorded here so that day is a conscious decision, not drift.

## Risks / Trade-offs

- [Risk] `podman exec` flakiness under load fails conducts that would otherwise start → Mitigation: preparation is 2–3 execs; failures tear down loudly rather than stranding sessions, and the live test guards the path.
- [Risk] Chowning an ancestor the image deliberately owns root (setuid helpers, system dirs under home?) → Mitigation: scope is strictly the target-parent chain below `/`, excluding mountpoints; image system dirs outside home are untouched. Tmpfs home content is per-session ephemeral anyway.
- [Risk] Future podman changes parent-creation ownership (e.g. creates as seat user) → Mitigation: conditional chown becomes a no-op naturally; mkdir -p stays harmless. The step degrades to redundant, never wrong.
- [Risk] RO-target parents gaining creation rights surprises authors → Mitigation: normative delta sentence — RO applies to the mounted subtree, which is never chowned; only the non-mounted ancestor chain gains traversal/creation rights as traversal infrastructure.

## Migration Plan

None. Per-session containers pick it up on next conduct; no units mutated in place, no profile changes, rollback is skipping the step.

## Open Questions

None. The spike (live reproduction) is done; mechanism verified against the real failure.
