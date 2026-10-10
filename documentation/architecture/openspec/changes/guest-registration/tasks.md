## 1. Registry schema and verification

- [x] 1.1 Registry config schema: single explicit file `${XDG_CONFIG_HOME}/cistella/registry.toml` with `$HOME/.config` fallback when XDG is unset/empty (nonempty XDG must be absolute — relative refuses, never cwd-relative; HOME unset/empty refuses); `format-version = 1` required; nofollow regular-file open with uid/mode fstat plus bounded read/parse on the same FD (no check-then-reopen gap); two tables `[[isolators]]` + `[[extensions]]` (genera never unified; companion pairs legal); exact name → exact role string, protocol major, sha256, source (fetch URL | absolute path), static argv; unknown keys refuse (no inert fields); per-entry key set pinned by unit test (one positive all-required-keys case, one negative unknown-key case); names nonempty with same-table duplicates refusing at parse; entries shadowing the table's reserved shipped name (`podman` in isolators, `landlock` in extensions) refuse at parse; malformed file refuses every conduct (shipped-only included — eager fail-closed); file must be seat-or-root-owned and not group/world-writable. No template expansion in any registry field (spans refuse at parse). Pin both swap cases: pathname swap after open parses pinned original (proceeds); in-place owner/mode change during read refuses.
- [x] 1.2 Fetch source: bounded bytes + deadline, private temp staging with atomic rename into the hash slot, digest-verify before first use, mismatch refuses pre-create typed; rotation requires registry edit (add fast unit pins, no network in tests — fixture bytes).
- [ ] 1.3 Path source: absolute-only parse refusal, full canonicalization with symlink-final refusal, seat-or-root ownership with writable-file/unsafe-parent/untrusted-owner refusals per the threat model, missing/non-executable refuses typed (mirror `discover_in` errors).
- [ ] 1.4 Same-FD anchor: digest computed over the nofollow-opened bounded FD whose object is exec'd (or private immutable copy + pinned FD); ownership/mode enforced on the open FD (only trusted seat/root may alter bytes after — explicit exception); swap-after-resolution executes pinned original bytes (prove live); cache hits re-verified every use. Pin swap-proceeds-pinned, poisoned-cache, and ownership-refusal regressions.
- [ ] 1.5 Role binding: table determines kind — profile `[isolator]` resolves only in `[[isolators]]`, `[[extensions]]` only in `[[extensions]]` (wrong-slot either direction refuses pre-spawn); recorded role string vs hello compared pre-planning (`podman` string in extensions refuses at parse; `landlock` string in isolators stays legal for the future sibling, guarded at runtime); zero-hook registered extension converges plain; shipped `landlock` semantics unchanged (declared-but-empty still refuses).

## 2. Resolution and hosting integration

- [ ] 2.1 Resolution order: shipped (`podman`/`landlock`, current sibling-relative path untouched) → registry → typed unknown-name refusal; empty/absent registry behaves exactly as 0.2.0 (prove with the existing closed-admission pins plus new registry tests).
- [ ] 2.2 Wire a registered extension through prepare and a registered isolator through lifecycle on a fixture guest (extend the deterministic peer or the new example guest below); divergence from shipped-path behavior is a conformance failure.
- [x] 2.3 Static argv only: registered args reach the guest spawn verbatim; session-context interpolation attempts refuse at parse (pin with a refusal test).

## 3. Builder example and docs

- [x] 3.1 Minimal example guest under `tests/fixtures/` (e.g. `tests/fixtures/example_guest/`; correct-behavior hello + prepare ack, not a fault injector): buildable via the declared `[[example]]` tables, exercised by the fast suite as a second conformance datapoint beside `fake_guest`.
- [ ] 3.2 `documentation/usage/profiles.md` registry reference: entry shape, fetch vs path, hash rotation procedure, typo-protection semantics; shipped guests documented as implicit entries and builders' reference.
- [ ] 3.3 Dogfood gate: repeat the unchanged-seat proof with one registered (path-source, in-seat copy of a shipped guest binary under a new name) profile before fleet exposure; live tier on host per 4.x precedent.

## 4. Validate and close

- [ ] 4.1 Validate: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, fast suite plus live tier on host; `openspec validate --all --strict`; sync and archive.
