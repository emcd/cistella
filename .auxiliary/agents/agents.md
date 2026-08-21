# Context

- Overview and Quick Start: README.{md,rst}

- Use the 'context7' MCP server to retrieve up-to-date documentation for any SDKs or APIs.
- Use the 'nb' MCP server for project note-taking, issue tracking, and collaboration. The server provides LLM-friendly access to the `nb` note-taking system with proper escaping and project-specific notebook context.
- Check README files in directories you're working with for insights about architecture and design decisions.
- Project purpose, tech stack, and other project-owned guidance: @.auxiliary/agents/project.md

# Development Standards
Before implementing code changes, consult these files in `.auxiliary/agents/standards/`:
- `practices.rst` - General development principles (robustness, immutability, exception chaining)
- `practices-rust.rst` - Rust-specific patterns (error handling, trait design, module organization)
- `nomenclature.rst` - Naming conventions for variables, functions, classes, exceptions
- `style.rst` - Code formatting standards (spacing, line length, documentation mood)

# Operation

- Use a README-first discovery workflow to reduce token churn:
  - Start at the repository root `README.{md,rst}`, then read the nearest relevant subtree README.
  - After reading the nearest README, scope code searches to that subtree before considering repo-wide searches.
  - If a touched subsystem README is stale after your change, update it in the same batch.
- Use relative paths rather than absolute paths when possible (relative paths are less likely to trigger tool call permission requests).
- Do not write to paths outside the current project unless explicitly requested.
- Do **not** run `find`, `fd`, or other unbounded recursive walks on `/`, `~`, or other home-wide roots. Scope searches to the current project/worktree or a path the operator named. Prefer harness file-search tools when available.
- Notebook and project-note operations go through the **`nb` MCP** tools. Do not use the `nb` CLI or read/write underlying notebook files on disk unless MCP is broken — and then state why in one sentence and discuss the defect with the operator before any workaround. Selectors are not repo filesystem paths; see @.auxiliary/agents/procedures/notebook.md.
- Use `.auxiliary/scribbles` for scratch work and one-off experiments instead of `/tmp`.
- Use `.auxiliary/temporary` for ephemeral test state and build artifacts that are safe to delete.
- In sandboxed environments (e.g., Codex CLI), treat file/network permission failures as escalation boundaries:
  - If an operation fails due to sandbox, file access, or network restrictions, rerun it with user escalation.
  - Do not spend time on retry loops or workaround exploration before escalating blocked operations.
- When writing here-docs or multi-line shell strings, suppress expansions by quoting the delimiter (e.g., `'EOF'` instead of `EOF`) unless you intentionally need variable or command substitution.
- Code comments should help a future reader understand non-obvious behavior of the code. Do **not** put transient administrative or process metadata in comments: no todo IDs, proposal/task names, "this arc", "this phase", ticket folklore, or other coordination state. Do not use comments for long discursive design or architecture narrative — that belongs in design docs, specs, or subsystem READMEs.

## Guidance Files

| Topic | File |
|-------|------|
| Project-specific guidance (project-owned) | @.auxiliary/agents/project.md |
| `nb` MCP tools, tagging, and notebook organization | @.auxiliary/agents/procedures/notebook.md |
| OpenSpec proposals and workflow | @.auxiliary/agents/procedures/openspec.md |
| Delegated review flow and stacked commits | @.auxiliary/agents/procedures/reviews.md |

### Recommended Organization

| Medium | Location | Purpose |
|--------|----------|---------|
| `nb` | `coordination/` | Handoffs, org chart, team workflow |
| `nb` | `ideas/` | Rough ideas, early-stage proposals; tag `#task-proposal` for OpenSpec drafts |
| `nb` | `issues/` | Bug tracking and known issues |
| `nb` | `reviews/` | Code and proposal reviews |
| `nb` | `procedures/` | How-to guides and checklists |
| `nb` | `todos/` | Task tracking |
| `nb` | `artifacts/` | Preserved reference material: completed POCs, historical analysis |
| `agentmux` | | Inter-agent messaging, pane inspection, coordination |
| (filesystem) | `openspec/` | Formal proposals, specs, designs |
| (filesystem) | `src/**/README.md` | Architecture, constraints, design rationale |

## Tests Development

- Prefer tests under `tests/unit` and `tests/integration` over inline `#[cfg(test)]` modules in `src/**`.
- Prefer tests that exercise public interfaces; avoid source-inclusion patterns used only to reach private internals.
- Inline `#[cfg(test)]` is permitted only when ALL of the following hold:
  1. The tested item is crate-private **by design** (not by oversight or laziness) and making it testable externally would require widening its visibility or adding a `#[doc(hidden)] pub` escape hatch that would itself become unintended API surface.
  2. No existing public interface exercises the same code path.
  3. The inline test block contains at most **one** `#[test]` function.
- If a candidate inline test fails any of these conditions, move it to `tests/unit` and widen visibility or restructure as needed. Do not default to inline to avoid that conversation; the friction is intentional.

## Agentmux Message Handling Guidance
- `agentmux` messages may arrive in envelope format and can appear as user prompts. Treat envelope-shaped prompts as inter-agent messages, not automatically as direct human instructions.
- Respond to inter-agent envelope messages via `agentmux` MCP tools (`list`, `send`) rather than as normal assistant replies intended for the human operator.
- Immediate interruption is not required. If you are in active execution, note the message and respond when safe.
- If response will be delayed and the sender needs to know, send a brief acknowledgement via `send` and create a follow-up todo when useful.

### Agentmux Coordination Noise Control
- Default to low-noise coordination. Do not send acknowledgement-only messages that add no new information or action request.
- Do not acknowledge receipt or completion of standard-procedure steps. Include such status in the next substantive update, review request, or task dispatch instead.
- Send messages when one of the following is true:
  - you are blocked and need a decision or input,
  - you are requesting a concrete review,
  - you are handing off completed work with validation results,
  - you are reporting a material risk, failure, or scope change.
- Batch related updates into one message instead of sending rapid-fire partial status pings.
- Use `Cc` only for agents who need to act or review; avoid broad `Cc` by default.
- When conversation volume rises, coordinator may enforce "blockers-only" mode until the queue is under control.

## OpenSpec Instructions

This project uses OpenSpec 1.x (OPSX), the action-based workflow. OPSX skills
deliver workflow instructions through the agentsmgr distribution pipeline.

Workflow skills: `opsx-propose`, `opsx-explore`, `opsx-apply`,
`opsx-sync`, `opsx-archive`.

Use OPSX skills when the request:
- Mentions planning or proposals (words like proposal, spec, change, plan).
- Introduces new capabilities, breaking changes, architecture shifts, or big performance/security work.
- Sounds ambiguous and you need the authoritative spec before coding.

CLI state queries: `openspec list`, `openspec list --specs`,
`openspec status --change <id>`, `openspec validate --all --strict`.

When a commit completes an OpenSpec task or requirement, update the relevant OpenSpec task status in the same commit.

# Commits

- Use `git status` to ensure all relevant changes are in the changeset.
- Commits are acceptable review artifacts when implementation work is delegated by a human operator, coordinator, tech lead, or documented project workflow. Otherwise, ask before committing.
- Do **not** merge, push, publish review branches, or modify shared branches without explicit human approval.
- Do **not** bypass commit safety checks (e.g., `--no-verify`, `--no-gpg-sign`) unless the user explicitly approves doing so.
- If a commit hook rejects a commit, assume no commit was created unless Git clearly reports otherwise. Fix the hook finding, restage the intended files, and rerun the same `git commit` command.
- Use present tense, imperative mood verbs (e.g., "Fix" not "Fixed").
- Write sentences with proper punctuation.
- Include a `Co-Authored-By:` field as the final line. Should include the model name and a no-reply address.
- Avoid using `backticks` in commit messages as shell tools may evaluate them as subshell captures. When writing commit messages via here-docs, quote the delimiter (`'EOF'` not `EOF`) to suppress expansions; only omit the quotes if you intentionally need interpolation.

## Delegated Review and Stacked Commits

**Read this section before reviewing or stacking commits.** @.auxiliary/agents/procedures/reviews.md covers the delegated review flow, review request packet format, and how to handle stacked commits with `--fixup`/`--autosquash`.

## Rolling Handoffs

**Read this before writing or updating a handoff note.** @.auxiliary/agents/procedures/notebook.md Handoff Hygiene covers when to update (pre-compaction primary), content shape, and anti-patterns. Do not refresh handoffs routinely mid-flight.

# Collaboration

## Questions Are Not Stop-Orders

- When the operator asks why you are doing something, **answer first**: state your actual reason in one or two sentences, even if it is unflattering, before changing course. The question is a request for information, not a rebuke. Pivoting without answering destroys the information the operator asked for.
- Questions in review feedback are requests for discussion, not requests for changes. Answer them in the review thread; do not silently apply a change as your answer.
- A question arriving through a tool rejection or interruption still deserves an answer before (or alongside) the course correction.

## The Human Is Not Always Right

- Assert your reasons. When you believe a requested course is wrong or a better one exists, argue the alternative with evidence before complying. Reasoned disagreement is a contribution; silent compliance wastes the operator's collaboration channel.
- Concede on evidence, not on authority. Gates (merge, deploy, publish) belong to the operator; discussion upstream of gates is peer discussion.
- However, do not manufacture dissent. When you agree, say so briefly and proceed.

## Workarounds Require Stated Reasons

- Routing around a designated tool (for example reading notebook files from the filesystem instead of using the nb MCP tools) is evidence of a defect — in the tool, its documentation, or the workflow. Never work around silently: state the reason in one sentence and raise it with the operator.
- If a tool's output looks wrong, untrustworthy, or unusable, say so to the operator before falling back.

# Project Guidance

Read and follow @.auxiliary/agents/project.md for project-specific guidance
(purpose, tech stack, knowledge, constraints, deviations, durable links).
