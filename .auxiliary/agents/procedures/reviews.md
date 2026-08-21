# Delegated Review Flow

Use this flow when multiple team members can access the same repository through branches or linked worktrees.

## Roles

- **Author:** implements the change, produces review packets, addresses findings with fixups, and prepares the cleaned stack for merge.
- **Reviewer:** performs the technical review (design, correctness, validation evidence, commit structure). One reviewer may cover the whole stack, or review may be split by commit/area; either way this is the technical gate.
- **Integrator** (coordinator or tech lead): accepts the **reviewer-approved, cleaned** stack for merge. Checks that the branch is based on the current `<local-integration-base>`, that the merge can proceed without conflict, and that human merge/push policy is respected. This is **not** a second technical review of the change content.

The same person may wear more than one role only when the project explicitly allows it. Do not treat "send to coordinator for merge" as another round of code review.

## Review cycle

1. Author implements the scoped change, runs validation, and creates local/private review commit(s) so the diff is hash-stable and hook-checked.
2. Author rebases onto the agreed `<local-integration-base>` and sends a review packet to the **reviewer**.
3. Reviewer approves or requests changes.
4. If changes are requested: author addresses them with fixup commits or first-class follow-up commits (see below), then sends an **updated** review packet. Return to step 3.
5. When the reviewer **approves**: author autosquashes targeted fixups into their targets.
6. **Base check after approval (required distinction):**
   - If `<local-integration-base>` is **unchanged** since the approved packet and only autosquash rewrote hashes: hand the cleaned stack to the **integrator** for merge (merge handoff with updated commit list; no repeat technical review).
   - If the base **advanced** and the author must rebase onto the new base — especially if conflicts are resolved — the result is a **changed technical artifact**. Send an **updated technical review packet** and return to step 3. Do not send a rebased stack straight to the integrator.
7. Integrator merges (see Integrator flow). Merge/push only after explicit human approval.

Hold the unsquashed fixup stack for the entire technical review. Autosquash only after reviewer approval, as the step that produces the stack the integrator merges when the base has not moved.

The agreed `<local-integration-base>` is a Git ref in the current repository, such as local `master` or a local lane integration branch. It is not a filesystem path and is not a remote-tracking ref. Do not use `origin/master`, another `origin/*` ref, a path like `/path/to/repo/master`, or a raw commit hash as the rebase base unless the coordinator explicitly names that exact ref or hash. When in doubt, ask for the local branch/ref name before rebasing. Confirm it with `git branch --list <local-integration-base>` or `git rev-parse --verify <local-integration-base>` before running `git rebase`.

## Integrator flow

1. Confirm the stack is reviewer-approved and already cleaned (fixups autosquashed). If fixups are still present, send it back to the author to fold before merge — do not start a content review.
2. Confirm the cleaned stack is based on the **current** `<local-integration-base>`. If the base has advanced (stale-base stack), **refuse the merge handoff** and route the author to rebase and return through **technical review** — do not independently assess the rebased content, and do not merge a stale-base cleaned stack.
3. If the base is current and the merge is otherwise clear, merge approved review branches with `--no-ff` when preserving a delegated-work or lane boundary; this creates a clear integration point and avoids mutually rebasing branches into increasingly long histories.
4. Merge/push only after explicit human approval.

Prefer reviewing commits by hash. Use an explicit worktree path only for uncommitted diffs or commits in a different repository. Use patch artifacts only as a fallback when the reviewer cannot access the repository, branch, or worktree directly.

# Review Request Packet

For non-trivial delegated work, review requests should include:

- Base ref for rebase: the `<local-integration-base>` to use for `git rebase` or `git rebase -i --autosquash`.
- Intended merge target: the branch/ref where the work should eventually land. This may differ from the rebase base, and may be a shared branch.
- Complete commit list with hashes and one-line descriptions.
- Validation commands run and results, including skipped checks or known gaps.
- Intended contract: what must be true after the change lands.
- Review concerns, if any: genuine uncertainty or risky areas only.
- Known risks, accepted tradeoffs, deferred items, or intentional branch staleness.

Author-provided review concerns are supplemental context, not a limit on review scope. Independent inspection remains the reviewer responsibility.

Packets to the **integrator** after approval are merge handoffs: cleaned commit list, validation status, and base/merge refs. They apply only when the base is unchanged since reviewer approval (autosquash-only hash changes are fine). They are not a second technical review packet. A rebase onto an advanced base requires a new technical review packet to the **reviewer**, not a merge handoff.

# Reviewing Stacked Commits

When feedback targets one specific commit in the current review stack, use `git commit --fixup <target-hash>`. This applies even when the review stack has only one commit. Do not directly amend reviewed commits while review is in progress; fixup commits preserve review visibility until the stack is ready for final cleanup.

A fixup is valid only when the entire fix belongs to code introduced by one target commit in the current stack. Use a first-class follow-up commit when the fix touches code that is already merged to the base branch, when the fix spans or refactors code introduced by two or more in-stack commits, or when the operator requests a distinct design change. Name the review finding or rationale in the first-class commit message or review reply so reviewers know why it is not a fixup.

The author holds targeted fixups in place until the **reviewer** approves. Each fixup stays visible on the branch in the context of its target commit, which preserves review visibility for the response. Do **not** autosquash before or during technical review — that rewrites hashes, stales the packet under review, and hides which fix addressed which finding. After reviewer approval, the author autosquashes into the target commits.

Distinguish post-approval hash changes:
- **Autosquash only, base unchanged:** include the new commit list and validation status on the merge handoff to the integrator. No repeat technical review.
- **Rebase onto an advanced base** (with or without conflict resolution): send an updated technical review packet to the reviewer and obtain approval again before any merge handoff. Do not treat a base rebase as equivalent to autosquash.

Fold the stack with `--autosquash`, which requires `-i` explicitly — `--autosquash` alone is a silent no-op. Use `<local-integration-base>` as the rebase base.

### Inspect before applying

`git log <local-integration-base>..HEAD` shows current commit order, not the autosquash-reordered todo. To print the prepared plan without applying it, point `GIT_SEQUENCE_EDITOR` at a small unique script file that prints the todo file contents to stderr and exits non-zero:

```sh
show_todo=$(mktemp)
printf '%s\n' 'cat "$1" >&2' 'rm -f "$0"' 'exit 1' > "$show_todo"
GIT_SEQUENCE_EDITOR="sh $show_todo" \
  git rebase -i --autosquash <local-integration-base>
```

This aborts before rewriting history and leaves no rebase state. Prefer a script file over nested-quote one-liners in `GIT_SEQUENCE_EDITOR`; agent harnesses often break the latter.

### Apply the fold

In agent environments, apply the prepared plan non-interactively:

```sh
GIT_SEQUENCE_EDITOR=true git rebase -i --autosquash <local-integration-base>
```

`GIT_SEQUENCE_EDITOR=true` accepts the autosquash-prepared todo unchanged and continues. It rewrites the branch; it is not a preview.

In an interactive human terminal you may instead run `git rebase -i --autosquash <local-integration-base>` and edit the plan in your editor.

If the result is wrong, recover with `git reset --hard ORIG_HEAD` — git sets `ORIG_HEAD` to the exact pre-rebase position regardless of how far back `<local-integration-base>` was.

For example, run `git rebase master` from the worktree branch when the coordinator says to rebase onto local `master`. Do not write `git rebase /path/to/repo/master`; that is a filesystem path, not a Git ref.

If `git commit` fails because a hook rejects it, assume no commit was created unless Git clearly reports otherwise. Fix the hook finding, restage the intended files, and rerun the same `git commit` command. Do not use `git commit --amend` to recover from a failed commit attempt.
