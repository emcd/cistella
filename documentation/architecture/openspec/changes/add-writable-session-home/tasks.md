## 1. Profile schema and authoritative descriptor

- [ ] 1.1 Add `home-source-class` (`container` | `host`, optional, absent means container) plus `home-host-source` path required exactly for `host` to `src/profile.rs` with validation and typed errors
- [ ] 1.2 Construct one authoritative home descriptor (canonical target + class + backing) at validation; bind it into `CompileSnapshot` as a fingerprinted field shared by supervisor derivation, retention sets, and mount rendering (replacing the hard `None` home source at the volume-args call)
- [ ] 1.3 Pin validation backend-free: absent defaults private, host-without-path refuses, unknown value refuses, host source with absent/container class refuses, host with path refuses as unsupported construction pre-mutation

## 2. Grant computation and consistency

- [ ] 2.1 Grant FULL on the home target for container-private class in `landlock_compiler.rs` and fold into `full_grant_routes` / `full_sources` accounting
- [ ] 2.2 Pin the grant set backend-free: home FULL present for container class, R+X-only for host class, RO-under-home retained `:ro`, contradiction preflight coverage for RW-beneath-RO under home
- [ ] 2.3 Pin snapshot/stale-plan consistency backend-free: descriptor mutation invalidates the fingerprint (recompute disagrees), and rendering from a mismatched snapshot refuses at application; grant/accounting consume the same descriptor (no compiler/renderer divergence)
- [ ] 2.4 Pin admission authority backend-free with adversarial controls: freshly rehashed invalid descriptor (container class + host backing) refuses; compile-only host fixture handed to create refuses; rendered backing/target differing from the admitted descriptor refuses; stale-hash case refuses — all before construction, staging, or guest-preparation side effects. Test actual supervisor argv/grant emission plus full_routes/retention/contradiction consumers from the descriptor, not manually injected FULL sets alone

## 3. Isolator interpretation

- [ ] 3.1 Thread the descriptor-derived home source through podman volume args (`--tmpfs` vs `--volume`) and quadlet unit rendering (`Tmpfs=` vs `Volume=`); host-class construction refuses until implemented
- [ ] 3.2 Full gates green (unit, fast suite, clippy, fmt, openspec validate) with no existing pin broken

## 4. Rollout

- [ ] 4.1 QA live proof on a podman seat: identify the actual private tmpfs HOME and its enforcement lineage; unmounted-home write succeeds when DAC permits; owner-writable nested RO source still shows an RO VFS bind with write denial and host sentinel unchanged (FULL is Landlock authorization, not a DAC/RO/capacity override). Plain/no-Landlock profiles get the host-construction refusal and gain no Landlock policy; no persistence proof inferred
- [ ] 4.2 Changelog entry under Unreleased describing the observable relaxation (not a harmless fix) plus the RO-parent/RW-child preflight remedy (move the graft); release vehicle 0.4.0 (minor: new capability + behavior change)
