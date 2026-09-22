# Trust model and limitations

A cistella seat runs with the operator's ambient authority. **It is not
a sandbox boundary.** Understand the following before handing a seat to
any agent, especially a lower-trust harness.

## Current contract

- **Whole-directory mounts carry secrets.** Seat profiles mount trees
  such as `~/.cargo` (registry tokens in `credentials.toml`),
  `~/.config/opencode` (provider keys in `auth.json`), `~/.ssh`
  (private keys), and the operator's notes and source trees. The agent
  inside can read everything mounted. This is accepted for single-user
  operation (the agent already acts with the operator's authority) and
  is the reason no per-file exclusion mechanism exists: exclusions
  would be brittle against upstream layout changes, silently breaking
  when a tool renames a file. Fleet operators opt into ambient
  authority with eyes open.
- **The harness needs its keys.** `auth.json` cannot simply be
  withheld — the harness calls providers with the operator's key. The
  trust contract is that the seat may touch what the operator may
  touch, not that secrets are sealed out.
- **Credential absence is about push credentials, not mounted data.**
  The `credential-surface` discipline (no code-hosting auth inside by
  default; sign-only agent socket for commits) and the template
  credential deny keep the driver from *adding* exfiltration paths,
  but they do not remove what whole-directory mounts already carry.
  Explicit `environment-acceptances` override this absence by
  operator choice (see below).
- **Nesting is fail-closed, not isolated.** Mounts nested under
  read-only ancestors must pre-exist on the host; the driver refuses
  rather than remounts. Kernel-enforced walls (user namespaces,
  read-only bind semantics) are the enforcement; the supplemental
  mechanisms below are future work, not promises.
- **Host sources must pre-exist.** The driver never creates host mount
  sources implicitly; missing sources are typed refusals.
- **Accepted environment is an explicit override, stored in the unit.**
  Variables listed in `environment-acceptances` cross the conduct
  boundary verbatim because the operator named them exactly —
  including secret-shaped names, which is deliberate, supported use
  and an explicit override to credential absence (credential-surface,
  assignments, and templates still inject no push credentials by
  default). Diagnostics display names only; the values themselves are
  stored in the unit file's `Environment=` lines for the session
  lifetime — visible to principals able to read or inspect them —
  and are torn down with the session. Acceptance grants no render
  permission, so accepted values cannot leak into mount paths,
  labels, or template diagnostics through the driver.

## Planned future work (not 0.1.0)

- **Supplemental confinement.** A Landlock layer inside the seat and /
  or synthetic writable views (FUSE / overlay staging) on top of the
  kernel walls. Either would narrow what a misbehaving harness can
  reach; neither replaces the ambient-authority contract above.
- **Preparation extension programs.** Caller- or profile-driven hooks
  that prepare mounts and environment before harness start (as
  opposed to today's fixed mountpoint-ownership preparation).
- **Seat-side validation.** The pre-push live gate currently requires
  Podman and systemd, so pushes must come from host shells; seats
  cannot self-certify. Options (documented, undecided) range from
  Podman-in-image to a codified host-push step.

No additional hardening on the current-contract fronts is planned
before 0.1.0; the contract above is the release posture. Changes to it
belong in a follow-up proposal, not in seat folklore.
