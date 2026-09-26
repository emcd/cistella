## 1. Guest hosting spine

- [x] 1.1 Implement sibling-relative guest discovery with typed pre-create refusal (no PATH search); state the trusted install-directory assumption and fail hello on guest/version mismatch.
- [ ] 1.2 Host external guests through the existing `GuestHost` (framed stdio, deadlines, kill/reap, pipe-EOF already owned there); add only what the wire path needs beyond it.
- [x] 1.3 Prove hosting-layer death semantics (guest-agnostic): kill a peer mid-exchange and pin bounded typed failure, clean shutdown after death, and fresh re-host with no wedged state. Op-level recovery (kill/re-exec converging by key at create/initiate; typed teardown after execute/await death) rides with the Podman guest in 2.3, where the operations exist.

## 2. Podman guest binary

- [x] 2.1 Ship the Podman isolator `--bin` speaking `isolator.*` ops over the framed protocol.
- [x] 2.2 Switch production `conduct` to the wire guest (live-proven 370/370: PTY+piped conduct, crash-after-create residue, orphan/gc, parallel load); fleet-deploy confidence comes later at the 4.1 dogfood gate, which this switch enables rather than precedes; the in-process impl stays as conformance reference. Session stdio crosses by explicit descriptor passing (`SCM_RIGHTS` bundle bound to the launch, PTY and piped modes preserved); the wire client SHALL check unit residue by reconciliation key after abnormal guest exit, SHALL fstat-and-hold only as defense-in-depth (fd identity is the proof), and supersedes `SessionPty` path-passing as the production mechanism (remove the path code rather than leaving a second route).
- [ ] 2.3 Pin wire/reference parity in conformance (divergence fails the suite), including op-level recovery: kill/re-exec converging by key at create/initiate, and typed teardown after guest death during execute/await.

## 3. Landlock guest binary and confinement

- [ ] 3.1 Ship the Landlock extension `--bin` answering `prepare` with contributions plus a guest-hook request.
- [ ] 3.2 Deliver the wrapper as exec ancestor with probe/apply and the dedicated diagnostics channel.
- [ ] 3.3 Translate host ancestor/subtree through the validated guest mount topology; refuse profiles with unaccounted aliases pre-create.
- [ ] 3.4 Prove `~/src` subtree confinement with the full denial matrix (write, create, unlink/rename, truncate at the required ABI minimum; fail pre-exec when unavailable) plus same-path pre/post restriction controls and an alternate-bind-path case.

## 4. Validation and docs

- [ ] 4.1 Dogfood repeat: unchanged-seat proof plus fleet sweep against the external path (QA tandem seat).
- [ ] 4.2 Validate: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, fast suite plus live tier on host; `openspec validate --all --strict`; sync and archive.
