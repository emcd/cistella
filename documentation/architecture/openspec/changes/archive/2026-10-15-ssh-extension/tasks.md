## 1. Shipped ssh guest

- [x] 1.1 New autobin `src/bin/cistella-extension-ssh.rs`: role `ssh`, `--socket=P --socket-canonical=C` argv (both host-supplied; guest's only fs read is the kind check), hello epoch/op gate, prepare epoch/op/session gates, contribution (env P, RO bind C→P, read permission on P, seat-socket credential for C), exit-2 refusals on missing/invalid argv.
- [x] 1.2 Unit pins: hello advertisement shape, prepare contribution byte-shape, refusal matrix (bad epoch, bad op, missing session, missing argv).
- [x] 1.3 Kind gate: `S_ISSOCK` (following metadata) on C required; non-socket refuses; env/container-target/permission name P, mount source and locator name C.

## 2. Driver selection + central merge

- [x] 2.1 `ExtensionResolution::Ssh` + `ExtensionSpawn::Ssh` on the standard shipped path (sibling install-dir trust, declaration order, no special-casing); driver reads ambient `SSH_AUTH_SOCK` at selection, canonicalizes C, builds argv; unset/missing/non-socket ambient with declared ssh refuses pre-create (declared-but-unprovidable).
- [x] 2.2 Parity pins: literal final argv for migrated agent/none profiles across 0/1/N extensions (non-symlink byte-identical; symlink `C:P` + declaration-order positions excepted as documented); ssh-vs-extension collision names both claimants; real landlock+tmpdir+ssh triple merges (fixture socket); real P!=C positive pin + host-C-as-permission negative pin.
- [x] 2.3 Slot ownership + bundle pins: other `SSH_AUTH_SOCK` claimants refuse under declaration; reservation holds undeclared; empty/divergent/extra ssh bundle refuses whole; `OnExtensions` rule matching `SSH_AUTH_SOCK` refuses a declared-ssh profile (uniform provenance, operator-ruled).
- [x] 2.4 Locator authority pins: non-prefix ambient socket admits by declared-locator equality; other guests' non-prefix handles still refuse; generic instance-bound mechanism.
- [x] 2.5 Namespaced instance IDs (`shipped:`/`registered:`, `:` refused in entry names) + `ssh` parse-time shadowing refusal; collision-control pins (binary-name entry disjoint, colon entry refused, no inheritance).
- [x] 2.6 Delete `credential-surface` (field, enum, `ssh_agent_volume_args`, join block, acceptance exception) + migrate all in-repo profiles; profiles naming the slot refuse at parse.

## 3. Validation

- [x] 3.1 Fast suite green both umasks; clippy/fmt/linecheck; validate --all --strict.
- [ ] 3.2 QA live tier: real triple + throwaway-`ssh-agent` forwarding (`ssh-add -l` through the socket), teardown-clean, missing/wrong-type/unset-ambient refusals, ambient-without-declaration control.
