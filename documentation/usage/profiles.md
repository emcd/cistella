# Profile setup reference

Profiles are TOML, resolved by name or explicit path. A name resolves in
order: `--configuration-directory <dir>` (flag beats
`$CISTELLA_CONFIGURATION_DIRECTORY`; each a closed tier naming
`<dir>/profiles/<name>.toml`), then
`${XDG_CONFIG_HOME}/cistella/profiles/<name>.toml` (`~/.config`
fallback, seeded from baked examples on reach — seed never overwrites
user files), then baked examples compiled into the binary
(`data/profiles/*.toml`; examples, not per-system profiles — see
`data/profiles/README.md`). A reference containing `/` or ending in
`.toml` is a filesystem path and bypasses every tier. Edit user copies
under XDG, never the baked files.

## Fields

```toml
image = 'localhost/cistella/opencode:example'  # required: tag or digest
credential-surface = 'none'                    # required; see below
container-home = '{{environment:HOME}}'        # default /home/cistella
command = ['opencode']                         # harness argv (array, never a shell string)
environment-acceptances = [                    # exact invoker-env names forwarded verbatim
  'AGENTMUX_BUNDLE',
]

[[mounts]]                                     # required (may be empty): allowlist triples
host-source = '~/.config/opencode'
container-target = '{{core:container-home}}/.config/opencode'
mode = 'ro'                                    # or 'rw'

[environment-assignments]                      # container env exports (template-resolved)
CARGO_HOME = '{{core:container-home}}/.cargo'

[labels]                                       # generic labels; `cistella.` prefix refused
'my.tag' = '{{core:project-name}}'

[isolator]                                     # backend selection (default podman)
name = 'podman'                                  # shipped podman, or a registry [[isolators]] name

[[extensions]]                                 # declared guests (default none = pure Podman behavior)
name = 'landlock'                              # shipped landlock, or a registry [[extensions]] name
```

- `container-home` is canonicalized; sensitive roots (`/`, `/etc` and
  friends) are rejected, as are non-absolute paths and control
  characters. Safe `.`/`..` segments canonicalize like literals.
- `environment-assignments` keys match `[A-Z_][A-Z0-9_]*`; values must
  be line-break-free (new in 0.1.1: `{{...}}`-free line-break rejection
  alongside the render gate). `HOME` is derived from
  `container-home`, never set here.
- `environment-acceptances` names exact invoker-environment variables
  to forward verbatim into container env. Every entry is required:
  absent names fail conduct before any session/runtime mutation with a
  name-only diagnostic. Collisions (assignment keys, `HOME`,
  credential-surface-injected names) are conduct-time errors. No
  template scanning applies to accepted values, acceptance grants no
  render permission (`{{environment:<name>}}` spans do not resolve),
  and no blacklist or credential-deny applies — an exactly-named
  acceptance is deliberate operator intent, including secret-shaped
  names (diagnostics display names only; values rest in unit
  `Environment=` lines, visible to principals able to read or inspect
  them — see the trust model).
- `credential-surface`: `"none"` mounts nothing;
   `{ ssh_agent = "/path" }` mounts a signing socket read-only so review
   commits can be signed without private key material entering the
   container. No authentication credential for code-hosting pushes
   belongs inside the container by default — explicit
   `environment-acceptances` are the operator override; host git
   operations use the operator's ambient authority outside the seat.
- CLI additions (conduct only): `--mount <host>:<target>:<mode>`
  (repeatable; exact profile-target matches override, partial overlaps
  fail), `--project-name`, repeatable `--supplement k=v`
  (last-wins), `--label k=v`, `--image` override.

## Template namespaces

Spans are `{{context:name}}` with `:` separator; bare `{{name}}` is a
typed error (no grandfathering). Contexts name the provider:

- `core:` — driver-resolved: `container-home`, `host-home`,
  `project-name` (`--project-name` flag, else the canonical session
  directory basename, validated and lazy).
- `supplement:` — caller-provided via `--supplement k=v`. Values are
  opaque until substitution; each sink applies its existing validator.
  Trusted, non-secret caller metadata: never bridge ambient secrets
  into supplements.
- `environment:` — host process environment under a compile-time
  allowlist (currently `HOME` only), with case-insensitive
  credential-deny (`SSH_AUTH_SOCK`, `*_TOKEN`, `*_SECRET`, `*_KEY`,
  `*_PASSWORD`, `*CREDENTIAL*`) checked before allowlist before
  lookup. This deny governs `{{environment:}}` templates only;
  explicit `environment-acceptances` are an operator override to
  credential absence (see the trust model). Diagnostics name only the variable, never its value. For
  example, a profile using `{{environment:GITHUB_TOKEN}}` fails with a
  typed credential refusal before any lookup — the token value (even
  if set) never appears in the error.

All three expand in mount triples (both sides), command argv, env
values, and labels values under one ordering / precedence / laziness /
no-rescan discipline. `container-home` additionally supports an early
expansion phase (allowlisted `environment:` + supplied `supplement:`;
`core:` spans there are self-reference errors), so
`container-home = '{{environment:HOME}}'` stays portable across seats.

## Guest registry reference

Third-party isolators and extensions resolve through one explicit
seat-local file: `${XDG_CONFIG_HOME}/cistella/registry.toml`
(`$HOME/.config` fallback when XDG is unset or empty), with
`format-version = 1` required. No layering, no URL bootstrap, no
profile-inline registrations: trust decisions get one audit point
per seat. Absent file means empty (0.2.0 behavior); malformed file
refuses every conduct pre-create, including shipped-only ones
(eager fail-closed — a silently-ignored registry could plain a
meant-hooked session). The file must be seat- or root-owned and
not group/world-writable.

```toml
format-version = 1

[[isolators]]                                  # genera table: resolves only from [isolator]
name = 'my-isolator'                           # exact name (nonempty, unique per table)
role = 'isolator'                              # exact hello role string the guest must advertise
protocol = 1                                   # wire major the guest speaks
source-digest = { algorithm = 'sha256', value = '<64 lowercase hex>' }
source-path = '/opt/guests/my-isolator'      # absolute path source (canonicalized at resolve)
arguments = ['--flag']                       # static arguments, passed verbatim (default [])

[[extensions]]                                 # genera table: resolves only from [[extensions]]
name = 'my-extension'
role = 'myext'
protocol = 1
source-digest = { algorithm = 'sha256', value = '<64 lowercase hex>' }
source-url = 'https://example.invalid/my-extension'  # fetch source (http/https; hash is the trust anchor)
```

- Entry shape: exact `name`, exact `role`, `protocol` major,
  `source-digest` (`algorithm` + hex `value`; sha256 only in v1), exactly one source
  static `arguments`. Unknown keys refuse (no inert fields — nothing
  unverified is assumable). `{{...}}` spans refuse in every field:
  no session, environment, or supplement values ever expand into
  registry fields.
- Fetch vs path: fetch downloads bounded (size cap plus deadline,
  at most 5 redirect hops, http or https — the hash anchors
  integrity, never TLS) into a content-addressed hash slot in a
  framework-owned cache (seat-owned non-symlink directory held
  open; temp create, chmod, rename, and slot open all run
  relative to that directory FD, so path substitution between
  check and use is structural, not checked), verified before first use and re-verified
  every use; path opens the absolute location nofollow with
  seat-or-root ownership, no group/other write bits, no
  world-writable ancestors (sticky exception: a sticky ancestor
  passes only when owned by the seat or root, since the directory
  owner can still swap entries), and symlink-final refusal,
  verified per conduct. Both refuse before any guest spawns on
  mismatch.
- Hash rotation: the slot is content-addressed, so rotation is a
  registry edit (new digest names a new slot) — upstream bytes
  never pick up silently. Replacing a path guest's bytes refuses
  at the next conduct until the registry hash follows.
- Typo protection: unregistered names still refuse (registration
  is opt-in per guest, never a widening of the default). Names
  shadowing a shipped guest (`podman` in `[[isolators]]`,
  `landlock` in `[[extensions]]`) refuse at parse; same-table
  duplicates refuse; a name registered in the other table refuses
  as a wrong-slot entry (the table determines the kind, then the
  hello role string compares pre-planning). At most one extension
  per profile in v1.
- Shipped guests are implicit entries: `podman` and `landlock`
  resolve sibling-relative as today with no registry involvement.
  Builders' reference: `tests/fixtures/example_guest/` (minimal
  correct hello plus prepare ack) beside the shipped binaries.
