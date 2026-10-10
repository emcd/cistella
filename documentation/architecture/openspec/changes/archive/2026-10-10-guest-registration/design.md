# Design: guest registration

## Context

0.2.0 hosts exactly two guests, both shipped in-crate and discovered sibling-relative to the
driver (`discover_in`: current-exe directory only, never PATH). Profiles name them
(`[isolator]`, `[[extensions]]`); anything else refuses pre-create (closed admission,
`guest-hosting` spec). The wire protocol already versions and capability-negotiates, and the
3.1 digest registry already pins hook artifacts by hash — registration extends both precedents
to separately-vended guests. Stakeholders: seat operators (registry authors), guest builders
(third-party isolators/extensions), reviewers (tier-1 + tier-2 for the trust surface).

## Goals / Non-Goals

Goals: third-party guests resolvable by exact registered name with digest-bound verification;
typo protection preserved (unregistered still refuses); zero migration for existing profiles;
a minimal example guest builders can copy.
Non-goals: pure-Landlock isolator sibling and shared Landlock code (later change, per todo);
signature verification (hashes first, signatures as a later upgrade); dynamic per-session
guest args (belongs to supplements, `cistella:todos/37`); seat-default tier design itself
(`cistella:todos/35` — registration ships its own explicit file now and must fit that tier
later, flagged as an open question).

## Decisions

- **Registry lives at seat scope, not in profiles.** Trust decisions (which third-party code
  a seat runs) get one audit point per seat instead of scattering across profiles. Rejected:
  profile-inline registrations (same guest re-verified per profile, no single audit point)
  and site-wide registry (root-owned, too coarse for per-seat experimentation).
- **Two source kinds, both exact.** Fetch (URL + sha256, downloaded to a framework-owned
  cache, content-addressed by hash, digest verified before first use) and absolute
  filesystem path (canonicalized, symlink-final refuses, digest re-checked at conduct).
  Rejected: PATH search, relative paths, globs (reopens the confusion the closed set
  closed), and trust-on-first-use (silent replacement).
- **Verify the bytes executed, not the bytes fetched.** The sha256 is computed over the
  SAME nofollow-opened, bounded regular-file FD whose object is exec'd (the `GuestHost`
  spawn pins its own FD after resolution), or over a private immutable verified copy
  whose pinned FD is exec'd. The open FD pins the inode, so a pathname swap after
  resolution is harmless by construction (the FD still names the verified bytes) and
  proceeds — but any reopened or swapped object re-verifies from scratch or refuses.
  In-place writes between hash and exec are closed by ownership, not by the FD: at open
  the file must be seat-or-root-owned and writable by neither group nor other (checked
  on the open FD's fstat), so only the seat operator or root — both trusted actors,
  stated as an explicit threat-model exception with rationale (the registry author is
  the seat operator; everyone else is refused by bits) — could alter bytes in the
  window. Cache hits are re-verified on every use (a hash-named mutable file is not
  authentication). Pinned in tests: swap-after-resolution executes the pinned original
  bytes (proved live), and untrusted-owner/writable files refuse at open (untrusted
  concurrent mutation is impossible by bits, not by timing).
- **Registry is one explicit seat-local file, now.** `${XDG_CONFIG_HOME}/cistella/
  registry.toml`, `format-version = 1` required (absent or otherwise-valued refuses;
  profiles adopt versioning separately in the 37/38 schema pass, not here). Single file,
  no layering in v1; independent of the unbuilt seat-default tier. It holds two tables —
  `[[isolators]]` and `[[extensions]]` — because isolators and extensions are two genera,
  never unified: a future `landlock` isolator registers beside the `landlock` extension
  as a companion pair, by design, and no cross-table uniqueness rule forbids that. What
  is forbidden per table: entries shadowing that table's reserved shipped name (`podman`
  in `[[isolators]]`, `landlock` in `[[extensions]]` refuse at parse — typo or confusion,
  never legitimate). A nonempty `XDG_CONFIG_HOME` must be absolute (relative refuses —
  a cwd-relative registry would let a repo-local file become an unreviewed registry);
  unset/empty falls back to `$HOME/.config` as profile resolution does. The file is
  parsed eagerly on every conduct: absent file means empty
  registry (exact 0.2.0 behavior, including for shipped-only profiles); malformed file
  refuses every conduct pre-create loudly, including shipped-only ones (a silently-ignored
  registry could plain a meant-hooked session — malformed is fail-closed everywhere, and
  this deliberate difference from absent is documented, not incidental).
  The file must be owned by the seat uid or root and not group/world-writable, else
  refuse — checked by fstat on the same nofollow-opened regular-file FD the bounded
  content is read and parsed from (opening to stat and reopening to read would admit a
  swap between check and parse). A pathname swap after open proceeds on the pinned
  original (same rule as guest binaries); an in-place owner/mode change of the opened
  file between open-fstat and post-read fstat refuses; symlink-final and non-regular
  objects refuse at open. The registry file itself is local only — entries may fetch binaries, but no
  registry URL or bootstrap fetch exists (circular trust refused by construction).
  Entries shadowing that table's reserved shipped name (`podman` in `[[isolators]]`,
  `landlock` in `[[extensions]]`) refuse at parse
  (a shadowing entry is a typo or a confusion attack, never legitimate).
- **Hash-pinning as the v1 trust anchor.** Matches the shipped-wrapper registry precedent;
  the hash is the trust anchor and transport is untrusted. No signature-shaped fields
  exist in v1 (see above); signatures arrive later as verifying fields or not at all.
- **Registry pins table plus role string; hello still negotiates.** The table determines
  the kind: `[[isolators]]` entries resolve only from the profile `[isolator]` slot and
  `[[extensions]]` only from `[[extensions]]` — an extension entry named in `[isolator]`
  (or vice versa) refuses pre-spawn before any host or spawn, with no hello exchange
  needed to catch it. Each entry then records the exact hello role string the guest must
  advertise (shipped `landlock` advertises `landlock` plus the `guest-hooks` capability —
  no generic `extension` marker exists on the wire and none is added); mismatch refuses
  pre-create pre-planning (confused deputy). A third-party extension
  registers its own role name and must advertise exactly it. Hooks are optional for
  registered extensions (prepare contributions still validate centrally); the shipped
  Landlock declared-but-empty refusal stays Landlock-specific. Protocol negotiation
  beyond the pinned major is unchanged — the wire already owns it.
- **Path ownership: seat-or-root, pinned.** Admit seat-uid- or root-owned regular
  executable files with registered digest + pinning (packaged root-owned binaries are
  legitimate; root outranks the seat user). Refuse otherwise-writable files, world-writable
  ancestor dirs, and other owners, per this explicit threat model (operator direction).
- **No signature field in v1.** An inert `signature` key the verifier ignores would let an
  operator believe a signature was checked. Omit it from the schema entirely; signatures
  arrive as a real, verifying field in a later change or not at all.
- **Fetch is bounded and atomic.** Max bytes + deadline on every fetch; stage to a private
  temp file and rename into the hash slot (no partial cache entries); the slot is still
  re-verified on every use.
- **Static args only in v1, parsed raw.** Registration may carry fixed argv (flags,
  subpaths), read from the raw registry config — never through profile template
  substitution; interpolation spans (`{{...}}`) refuse at parse and no session or
  environment values expand (the prepare-payload precedent: session-blind extension args
  are never legitimate). Dynamic per-session data waits for supplements.
- **Shipped guests become implicit entries.** `podman`/`landlock` resolve exactly as today;
  the registry file is parsed eagerly for every conduct but entries resolve only for
  non-shipped names. Empty registry behaves exactly like 0.2.0.

## Risks / Trade-offs

- [Risk] Fetch URL serves attacker bytes → Mitigation: bytes are untrusted until the
  registered hash verifies; mismatch refuses pre-create, nothing executes.
- [Risk] Path guest replaced between conducts → Mitigation: digest re-checked at conduct
  against the registered hash; replacement refuses instead of running.
- [Risk] Registry file itself tampered → No regression: seat-owned config, same trust as
  profiles today (an attacker who can write the registry can already write profiles).
- [Risk] Stale cached fetch after upstream rotates → Mitigation: cache keyed by hash, so
  rotation requires a registry edit (explicit operator intent), never silent pickup.

## Migration Plan

Additive only: new config schema (unknown tables still refuse), new resolution path behind
the existing unknown-name refusal. Rollback is commit revert — with no registry entries,
all non-shipped names refuse exactly as in 0.2.0. Profiles, sessions, and state untouched.

## Open Questions

- Seat-default tier fit (`cistella:todos/35`): the explicit single-file schema defined here
  must fit wherever that tier lands — flag at review if it conflicts.
- Fetch cache eviction bounded by disk hygiene (`gc` coverage or TTL?) — default to
  unbounded + `gc` in v1, revisit on evidence.
