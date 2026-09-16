## 1. Template expansion core

- [ ] 1.1 Split `Profile::from_toml` into parse (+ `~` expansion) and validate; add `expand_templates` over triples + command argv run between them during resolution (`{{container-home}}` from canonical `container_home`, `{{host-home}}` from `$HOME`, `{{project-name}}` from context; unknown names error; single pass, no rescan). `{{...}}` in `container_home` itself is rejected before canonicalization/derivation
- [ ] 1.2 Validate `--project-name` charset (alphanumeric plus `-_.`, mirroring agentmux) before expansion; rejection is a typed error, never sanitization
- [ ] 1.3 Unit tests: each template on both triple sides + argv, default project from directory basename (lock `/home/me/src/CLONES/cistella/qa` → `qa`, then `--project-name cistella` override), flag override, unknown template error, container_home-template rejection, no-rescan guarantee, expanded values re-validated (overlap/injection checks see final text)
- [ ] 1.4 Resolution-context boundary tests: `resolve_in` takes the project name explicitly (conduct passes basename-or-flag after canonicalization); context-free `resolve` handles template-free profiles literally and fails template-bearing ones — never cwd-derived

## 2. CLI plumbing

- [ ] 2.1 Add conduct-only `--project-name <name>`; default to basename of canonical session directory (computed after canonicalization, threaded into resolution)
- [ ] 2.2 Convert `data/profiles/opencode.toml` targets to `{{container-home}}/...`; seat `opencode` profile gains `Notes/{{project-name}}` RW triple (verify live)
- [ ] 2.3 Agentmux coder entry gains `--project-name {{project-name}}` once implemented (operator-side edit, verified with `check configuration`)

## 3. Validation

- [ ] 3.1 Run `cargo clippy --all-targets -- -D warnings` and `cargo nextest run --config-file .auxiliary/configuration/nextest.toml` green (fast + live); `openspec validate --all --strict` green
