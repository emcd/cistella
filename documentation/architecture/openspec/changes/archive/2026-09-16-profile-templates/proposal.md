## Why

Profile mount triples repeat literal home paths (`/home/cistella/...` nine times in the dogfood profile) and cannot express project-keyed mounts: the notebook RW triple needs the project name, which varies per session. Agentmux 0.10.2 already interpolates `{{project-name}}` on its side with nothing to receive it on ours. Templates remove the duplication and retire the per-session `--mount` workaround for project paths.

## What Changes

- Three template expansions in profile `[[mounts]]` (both sides) and `command` argv: `{{container-home}}` (the profile's canonical `container_home`), `{{host-home}}` (the invoking seat's `$HOME`), and `{{project-name}}` (see below). Existing leading-`~` expansion stays (backcompat); templates generalize it to any position.
- New conduct-only `--project-name <name>`: explicit project name. Default is the basename of the canonical session directory (matching agentmux's default, so the two agree without configuration).
- Unknown `{{...}}` names are typed errors; substituted values are never rescanned (a project name containing brace-shaped text cannot inject a second expansion).
- `data/profiles/opencode.toml` adopts `{{container-home}}` (proving templates in shipped examples); the seat `opencode` profile adopts a `~/Dropbox/Notes/{{project-name}}` RW triple.

## Capabilities

### New Capabilities

(none — all changes modify the existing mounts contract)

### Modified Capabilities

- `mounts`: template expansion in profile parsing (timing, precedence, errors) with new scenarios; `--project-name` CLI flag.

## Impact

- `src/profile.rs` (parse → expand → validate split; `Profile::resolve` gains project-name input), `src/cli.rs` + `src/main.rs` (flag, defaulting, threading), `data/profiles/opencode.toml` (template adoption).
- Agentmux (consumer): `{{project-name}}` in coder templates finally has a receiver; per-project `--mount` flags retire for notebook paths.
- Out of scope (separate todos): hyphenated keys (`todos/profiles/5`), `[environment]` rename (`todos/profiles/4`) — land together later so profiles break once. No validation-semantics change: expanded values pass through the existing checks unchanged.
