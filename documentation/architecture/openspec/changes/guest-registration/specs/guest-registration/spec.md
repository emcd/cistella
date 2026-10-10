# guest-registration Specification

## Purpose
Third-party guest registry (registration replacing closed admission): exact-name resolution
with digest-bound verification, seat-scoped, typo-safe by default.
## ADDED Requirements
### Requirement: Explicit seat-local registry file

Resolution SHALL consult exactly one registry file at `${XDG_CONFIG_HOME}/cistella/
registry.toml`, falling back to `$HOME/.config/cistella/registry.toml`
when `XDG_CONFIG_HOME` is unset or empty (same fallback as profile resolution — an unset
XDG var is normal-seat condition, never an error). The file SHALL declare
`format-version = 1`; absent or otherwise-valued versions SHALL refuse. A nonempty `XDG_CONFIG_HOME` SHALL be
absolute (XDG requires absolute paths); a relative value SHALL refuse with a typed error
(a relative base would resolve the registry against the conduct worktree, letting a
repo-local file become an unreviewed registry — the registry path never depends on cwd).
A `HOME` that is itself unset or empty SHALL refuse with a typed error, since no seat path
can be formed. No layering, no URL, no bootstrap fetch exists in v1. The registry file
SHALL be opened nofollow as a regular file, with uid/mode checked by fstat on that same
FD and the bounded content read and parsed from that same FD; symlink-final, non-regular,
or metadata-swapped objects SHALL refuse (opening to stat and reopening to read would
admit a swap between check and parse — the same gap the binary-source anchor closes). An absent file
SHALL mean an empty registry (exact 0.2.0 behavior). A malformed file SHALL refuse every
conduct pre-create loudly (a silently-ignored registry could plain a meant-hooked
session). The file SHALL be owned by the seat uid or root and not group/world-writable;
otherwise conduct SHALL refuse. Unknown registry keys SHALL refuse at parse (no inert
fields — nothing unverified is assumable).

#### Scenario: Absent registry is 0.2.0 behavior
- **WHEN** no registry file exists and a profile names only `podman`/`landlock`
- **THEN** conduct proceeds exactly as without registration (sibling-relative discovery, closed admission otherwise)

#### Scenario: Unset XDG falls back to HOME config
- **WHEN** `XDG_CONFIG_HOME` is unset or empty and the registry lives at `$HOME/.config/cistella/registry.toml`
- **THEN** conduct resolves through the fallback path exactly as through the XDG path

#### Scenario: Unset HOME refuses typed
- **WHEN** `HOME` is unset or empty (regardless of `XDG_CONFIG_HOME`)
- **THEN** conduct refuses pre-create with a typed error before any guest resolution

#### Scenario: Relative XDG refuses typed
- **WHEN** `XDG_CONFIG_HOME` is nonempty but relative (e.g. `config`)
- **THEN** conduct refuses pre-create with a typed error; no fallback, no cwd-relative registry

#### Scenario: Registry pathname swap after open parses pinned original
- **WHEN** the registry path is replaced on disk after the nofollow open but before parse completes
- **THEN** conduct parses the pinned original bytes and proceeds (the open FD names them regardless of the path); the replacement is admitted only at the next conduct, when it is eagerly read and revalidated

#### Scenario: Registry swap before open validates the new file
- **WHEN** the registry path is replaced before conduct opens it
- **THEN** conduct validates the new file normally (ownership, mode, digest-shape, schema) and proceeds or refuses on its own merits

#### Scenario: Opened registry mutated during read refuses
- **WHEN** the opened registry file's owner or mode changes between open-fstat and post-read fstat
- **THEN** conduct refuses with a typed error (content torn-reads are covered by the trusted-owner exception: only seat/root could have written, per the ownership gate)

#### Scenario: Malformed registry refuses everything
- **WHEN** the registry file fails to parse
- **THEN** conduct refuses pre-create with a typed error before any guest resolution; no session starts

#### Scenario: Malformed registry refuses shipped-only sessions
- **WHEN** the registry file fails to parse and the profile names only shipped guests
- **THEN** conduct still refuses pre-create (eager fail-closed: malformed is never silently ignored, unlike absent)

#### Scenario: Bad registry permissions refuse
- **WHEN** the registry file is group/world-writable or owned by another uid
- **THEN** conduct refuses pre-create with a typed ownership error

#### Scenario: Shadowing entry refuses at parse
- **WHEN** a registry entry is named `podman` in `[[isolators]]` or `landlock` in `[[extensions]]`
- **THEN** resolution refuses with a typed error before any guest is touched (reserved shipped names are never shadowed; a `landlock` entry in `[[isolators]]` remains legal for the future sibling)

#### Scenario: Duplicate name within a table refuses at parse
- **WHEN** two entries in the same table share a name
- **THEN** conduct refuses with a typed error before any guest resolution (first-wins would hide typos; shadowing across tables is covered separately)

#### Scenario: Empty entry name refuses at parse
- **WHEN** a registry entry declares an empty name
- **THEN** conduct refuses with a typed error before any guest resolution (names are nonempty, exact-match keys)

### Requirement: Verify the bytes executed

Digests SHALL be computed over the SAME nofollow-opened, bounded regular-file FD whose
object is exec'd (or over a private immutable verified copy whose pinned FD is exec'd).
The open FD pins the inode, so a pathname swap after resolution proceeds on the verified
bytes by construction — but any reopened or swapped object SHALL re-verify from scratch
or refuse. In-place writes between hash and exec are closed by ownership: at open the
file SHALL be seat-or-root-owned and writable by neither group nor other (checked on the
open FD), so only the trusted seat operator or root could alter bytes in the window
(explicit threat-model exception — everyone else is refused by bits). Cache hits SHALL be
re-verified on every use (a hash-named mutable file is not authentication). Fetch and
path sources share this anchor rule: fetch verifies the staged bytes before first use and
re-verifies the slot per use; path verifies the resolved FD per conduct.

#### Scenario: Swap after resolution executes pinned bytes
- **WHEN** a registered path is replaced on disk after resolution but before spawn
- **THEN** conduct proceeds executing the verified pinned bytes (the open FD names them regardless of the path); the swapped bytes never execute without a fresh verify

#### Scenario: Untrusted concurrent mutation impossible by bits
- **WHEN** a registered file is seat-or-root-owned and not group/world-writable at open
- **THEN** no untrusted actor can alter bytes before exec (only owner-or-root may chmod, chown, or write — both trusted); ownership/mode violations refuse at open instead

#### Scenario: Poisoned cache refuses per use
- **WHEN** a hash-keyed cache slot no longer matches its hash at conduct time
- **THEN** conduct refuses with a typed verification error instead of executing the poisoned bytes

### Requirement: Fetch source verification

Fetch sources SHALL declare a URL plus sha256, bounded by max bytes and a deadline, staged
to a private temp file and renamed into the hash slot atomically (no partial entries).
Both `http` and `https` URLs fetch (transport is untrusted by design — the registered
hash anchors integrity, never TLS); redirects follow at most 5 hops, then refuse.
Mismatch SHALL refuse pre-create and execute nothing. Upstream rotation SHALL require a
registry edit (new hash), never silent pickup. The framework-owned cache directory SHALL
be a seat- or root-owned non-symlink directory opened and verified via its own FD
(mode hardened to 0700 on the FD, never by path); stage and slot operations (temp
create, chmod, rename, slot open) SHALL run directory-FD-relative to the held
description, so no path operation can be substituted between check and use. A present
slot SHALL be re-verified on every use; a poisoned or symlinked slot SHALL refuse with
a typed verification error instead of refetching.

#### Scenario: Digest mismatch refuses pre-create
- **WHEN** fetched bytes do not match the registered sha256
- **THEN** conduct refuses pre-create with a typed verification error; no guest executes

#### Scenario: Rotation requires registry edit
- **WHEN** upstream publishes new bytes under a previously registered URL
- **THEN** conduct keeps using the hash-pinned cache (or refuses if uncached) until the registry hash is updated

### Requirement: Path source verification and ownership

Path sources SHALL be absolute; relative paths, PATH search, and globs SHALL refuse at
parse. The framework SHALL canonicalize fully with symlink-final refusal; missing or
non-executable paths SHALL refuse pre-create typed. Admitted files SHALL be regular
executables owned by the seat uid or root (operator direction — root outranks the seat
and packaged root-owned binaries are legitimate); otherwise-writable files, world-writable
ancestor dirs, and other owners SHALL refuse per this threat model — with one reasoned
exception: a world-writable ancestor WITH the sticky bit passes only when its owner is
trusted (seat uid or root). Rationale: sticky restrains non-owners, but the directory
owner may still remove or rename anyone's entries — so a sticky dir owned by another uid
refuses exactly like a non-sticky world-writable dir. `/tmp` (root-owned, mode 1777)
passes; the exception admits exactly the configurations where no untrusted actor can
swap the entry, and stays load-bearing for seat scratch roots.

#### Scenario: Replaced path guest refuses at conduct
- **WHEN** a registered path's bytes no longer match the registered hash at conduct time
- **THEN** conduct refuses with a typed verification error; the replaced binary never executes

#### Scenario: Relative path refuses at parse
- **WHEN** a registry entry names a relative path
- **THEN** resolution refuses with a typed error before any guest is touched

#### Scenario: Untrusted owner refuses
- **WHEN** a registered path is owned by another uid, group/world-writable, or under a world-writable ancestor
- **THEN** conduct refuses with a typed ownership error before verification

### Requirement: Registered role binding

Each entry SHALL live in exactly one table — `[[isolators]]` or `[[extensions]]` — and
the table determines the kind: `[[isolators]]` entries resolve only from the profile
`[isolator]` slot and `[[extensions]]` only from `[[extensions]]`. Each entry SHALL
further record the exact hello role string the guest must advertise (shipped `landlock`
advertises `landlock` plus the `guest-hooks` capability — no generic `extension` marker
exists on the wire and none is added). The profile slot table SHALL agree with the
registry table before spawn; the recorded string SHALL then be compared against the
hello advertisement pre-planning.
Either disagreement SHALL refuse (a registered extension named in `[isolator]` refuses
even if its hello string would match elsewhere — the hello string alone cannot catch a
wrong-slot entry). Hooks SHALL be optional for registered extensions (prepare
contributions still validate centrally); the shipped Landlock declared-but-empty refusal
stays Landlock-specific. Protocol negotiation beyond the pinned major SHALL proceed
exactly as the wire specifies.

#### Scenario: Role mismatch refuses pre-create
- **WHEN** a guest registered with one role string advertises another at hello
- **THEN** conduct refuses pre-create with a typed role error before any planning

#### Scenario: Extension entry in isolator slot refuses pre-spawn
- **WHEN** a profile names a `[[extensions]]` registry entry in the `[isolator]` table
- **THEN** conduct refuses pre-create with a typed slot error before any host or spawn, regardless of hello strings

#### Scenario: Isolator entry in extension slot refuses pre-spawn
- **WHEN** a profile names a `[[isolators]]` registry entry in `[[extensions]]`
- **THEN** conduct refuses pre-create with a typed slot error before any host or spawn, regardless of hello strings

#### Scenario: Role string shadowing the isolator name refuses at parse
- **WHEN** a `[[extensions]]` entry records hello role string `podman`
- **THEN** conduct refuses with a typed error at parse, before any slot check or hello (no podman extension exists or is planned — this is typo or confusion, never legitimate; note the asymmetry is deliberate: a `landlock` string in `[[isolators]]` stays legal for the planned future sibling, guarded at runtime by the slot check plus the string compare)

#### Scenario: Role string `landlock` in `[[isolators]]` proceeds to runtime guards
- **WHEN** a `[[isolators]]` entry records hello role string `landlock`
- **THEN** conduct does not refuse at parse; the slot check (profile `[isolator]`) and the string compare apply at runtime as usual

#### Scenario: Zero-hook registered extension converges
- **WHEN** a registered (non-`landlock`) extension answers prepare with zero hooks
- **THEN** conduct proceeds on the plain path with its env/mount contributions merged (no refusal; hooks were never promised)

### Requirement: Static argv parsed raw

Registry argv SHALL be read from the raw registry config and passed to the guest spawn
verbatim. Interpolation spans (`{{...}}`) SHALL refuse at parse; no session, environment,
or supplement values SHALL expand into guest argv. A span-shaped static arg SHALL refuse
rather than expand.

#### Scenario: Interpolation span refuses at parse
- **WHEN** a registry entry carries `{{supplement:x}}` (or any span shape) in argv
- **THEN** resolution refuses with a typed error before spawn; nothing expands
