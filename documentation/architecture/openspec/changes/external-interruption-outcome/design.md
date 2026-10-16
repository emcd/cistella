## Context

QA's live boundary (0/10 at `a00c882`) plus the baseline comparison
arm (identical 137/255 on `baa7b98` product) establish the exec-255 /
designed-137 interleaves as pre-existing fidelity behavior, newly
exposed by tap diagnostics — not a shape change from the unlink fix.
Cleanup converges in every observed run (unit gone; service
inactive/not-found; container absence recorded as generic-nonzero,
with code-exact exit-1 capture adopted for future runs). The
fixed-143 premise is withdrawn. Baseline comparison does NOT
reproduce the unlink race (no ENOENT observed); that race rests on
the source interleave + unit witness. Raw wait values (e.g. 137<<8)
encode client exit statuses, never observed harness signals. Exit 1
as a driver-report class is a typed-report/provenance distinction,
not a disjoint integer (a harness can itself exit 1).

Snapshot limits (Tier-2, accepted): corroboration snapshots CANNOT
distinguish "harness exited 255, then another actor removed the
container before the query" from true mid-await interruption, and
later cleanup absence cannot upgrade the hypothesis. No host surface
reads the guest execution table (todos/49). The generated
`ExecStop: podman rm -f` line is stop-path provenance evidence for
the 137 class, not a per-run harness raw-wait claim. `podman exec`
stderr is harness-passed stdio in BOTH Inherit and HeldFiles modes;
with zero transport changes there is NO product-owned client-stderr
capture on any path (TTY or otherwise) — test-side taps stay
test-side.

## Goals / Non-Goals

- Goals: an observational (never causal) typed outcome; two-phase
  ownership with proof dominance; ordinary semantics preserved via
  negative controls including post-completion-removal and
  concurrent-stop shapes; capability-gated wire with exact 4-combo
  outcomes; byte-safe pinned rendering.
- Non-Goals: no causal interruption claims (`ContainerRemoved` /
  `StopKill` as proven cause are WITHDRAWN); no product stderr-tail
  capture; no bare allow-list; no fabricated dispositions; no
  unlink-repair change; no binding surface (todos/49).

## Decisions

- Observational report, option (a): the variant reports
  co-occurrence facts — client completion + container absent at
  query + service state at query — and declares the harness outcome
  unobservable. It NEVER claims proven external cause. Cleanup
  confirmation never upgrades the causal hypothesis. (Option (b), a
  real per-execution correlated event surface, is todos/49-adjacent
  future work, explicitly out of scope.)
- Two-phase ownership unchanged: backend `Unobservable` (facts +
  raw, no cleanup claim; await never does teardown); framework
  finalizes post-proof or yields to dominant residue /
  observation-error / shutdown-uncertain. `cleanup converged`
  prints only after actual final proof.
- Proof dominance, no fallthrough: ANY teardown error dominates
  (death/uncertainty keep recorded text inside the report);
  release failure dominates next; backend absence is then
  re-proven by a backend-owned `prove_absence` (container, service,
  unit re-queried — never inferred from earlier snapshots), whose
  converged bit (strict exit-1 + non-active service + absent unit)
  alone gates `cleanup=converged`. Observation errors and
  unconverged proofs dominate with their own reports.
- Generality split: the framework owns taxonomy types only;
  Podman-domain interpretation (exists codes, ActiveState rule,
  absence-proof rule) lives in `isolators::corroboration`, the
  completion freeze in `isolators::podman_await`; the framework
  routes outcomes and renders bits, never backend states.
- No condition enum with causal names: the observation record
  carries the corroborated snapshot (container-exists code,
  service states, unit presence, query ordering) instead of
  `ContainerRemoved`/`StopKill` verdicts. The normal-255-then-
  removed-before-query shape yields a TRUE report (the container
  WAS absent at query; the outcome IS unobservable) — honesty by
  construction, since nothing causal was claimed.
- Negative controls: ordinary Exited 137/143/255 with container
  present AND service active (exact dispositions);
  spoofed-text-255 present-and-active (Exited(255));
  signaled-with-container-present-and-active keeps legacy
  `Signaled` mapping by explicit compatibility policy (the client
  faithfully reported its completion; harness receipt of any
  signal is unproven and unclaimed — preserved byte-for-byte, not
  upgraded, so implementation has exactly two branches and invents
  no third unknown-variant); service-inactive (stop in flight)
  with EITHER container code reports observably — mere existence
  covers stopped-but-present containers, so existence alone never
  gates (QA Mode B: Exited-137 with present container but
  deactivating service);
  normal-nonzero followed by post-completion removal (observational
  report, explicitly NOT a proven-interruption claim);
  unrelated-signal during concurrent stop (no causal upgrade).
- No product stderr tail: with zero transport changes, raw-tail
  availability follows owned capture capability, which is NONE on
  product paths (inherited/passed streams, TTY and non-TTY alike).
  The outcome carries raw status only; tails live in test-side taps
  (boundary evidence), never promised in product. If a future
  owned capture path appears, it gets its own proposal.
- Completion freeze: exactly one corroboration per execution —
  the first awaiter observing completion claims the child and
  owns caching; losers replay the frozen cache (bounded,
  cancel-aware), never re-corroborate. Pinned by a
  barrier-synchronized two-awaiter race plus cache-replay
  assertion (inline: tables are crate-private by design).
- Wire compat by capability, exact outcomes per pair shape.
  Wire sessions (hello negotiated): capability negotiated both
  ways → typed variant; any of the other three wire combos (new
  host/old guest, new/new WITHOUT the negotiated capability,
  old/old) → legacy mapping on both sides (documented current
  behavior); the old-host/new-guest combo is safe by guest-side
  obligation (the guest MUST NOT emit without negotiation, AND
  advertises the capability ONLY when offered — an unsolicited
  advertisement would refuse old-host admission before downgrade
  matters). Negative path (not a normal combo): guest emits the
  variant without negotiation → host refuses. In-process reference
  backend has no hello: it implements the typed shape natively
  (nothing to negotiate locally); conformance pairs it with wire
  guests that negotiated the capability, and separately pins
  legacy-mapping parity for unnegotiated pairs. Unknown
  tags/fields/bounds refuse; observed outcomes replay until
  `remove`.
- CLI rendering (one line, pinned goldens): `error: execution
  outcome unobservable (<observation>): raw client
  <exited(code)|signaled(n)>; cleanup=<converged|dominant-error>`.
  Exit 1, driver-report class. The container fact renders from the
  recorded code (absent only for exit 1); no tail field in product
  rendering (none available); test failures render tap tails
  separately.
- Schema closure: exactly one known top-level variant, closed
  record (mixed tags and extra fields refuse); typed raw
  `exited(0)` refuses (success never classifies unobservable).
- Transition precedence: preexisting hooked
  exec-failure/transition dispositions run BEFORE the
  unobservable arm (a failed transition owns its report even
  when completion is also ambiguous).

## Risks / Trade-offs

- [Observation reports feel weaker than verdicts] → Mitigation:
  weakness stated, not hidden; the report is always TRUE (facts +
  explicit non-claim), where a causal verdict would sometimes be
  false. Users get cleanup proof + raw evidence, which is what
  diagnosis needs.
- [Stop-path killers want a StopKill label] → Mitigation: denied
  as a causal verdict (snapshots cannot prove it); the observation
  record includes service state at query, and any future correlated
  stop-event surface upgrades precision without changing the shape.
- [Capability matrix load] → Mitigation: 4 exact combos as
  protocol-peer unit tests, no live matrix.

## Migration Plan

Packet → Tier-1 → Tier-2, implement observation record +
corroboration + mapping + boundary reshape, QA live loop, sync
both deltas + changelog. Unlink repair sync proceeds independently.

## Open Questions

- None blocking; rendering goldens confirm byte-safety in review.
