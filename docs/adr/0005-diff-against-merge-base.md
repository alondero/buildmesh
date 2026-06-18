# 5. Diff Changed Files Against The Merge-Base, Not HEAD

Status: accepted

The Changed-Files / diff views compute an Agent Node's changes against the **merge-base of its worktree `HEAD` and the mesh's `base_ref`** (default `origin/main`), not against `HEAD`. The "old" side of every diff is the merge-base tree; the "new" side is the working directory, so a file is shown as changed if the agent touched it **at any point since branching** — whether the change is committed or still uncommitted.

## Context

`diff_file_against_head` (and the Changed-Files list, the mobile `/api/agents/{id}/diff` route, and the per-file `+/−` summary that all sit on top of it) diffed the working tree against `HEAD`. That answers *"what is uncommitted right now?"* — but it is the wrong question for reviewing an agent.

Buildmesh agents run in `git worktree`s that are usually created **detached at `base_ref`** (ADR 0003; `base_ref` defaults to `origin/main`, is stored on the `meshes` row, and is mirrored into `.claude/settings.json` `worktree.baseRef`). An agent is free to commit as it works. The moment it does, those committed files **drop out of the HEAD diff** — the cornerstone "here's what this agent changed" view silently shrinks to only the not-yet-committed tail. For a detached worktree `HEAD` *is* the agent's own latest commit, so "vs HEAD" can mean "vs my own work," which is nearly empty.

The mental model users actually have — the one GitHub's "Files changed" tab serves — is **"everything this branch did relative to where it started."** That is a diff against the merge-base with the Base Ref, including both committed and uncommitted work.

## Decision

1. **Merge-base is the old side.** Resolve the node's mesh `base_ref` to a commit, compute `merge_base(HEAD, base_commit)` via `git2`, and use that commit's tree as the diff baseline. Diff it against the **working directory** (`diff_tree_to_workdir_with_index`) so uncommitted edits still appear.
2. **One call returns the whole change set.** A single `diff_node_against_base(node_id) -> DiffResult` enumerates every changed file with per-file status (`A/M/D/R`), `+/−` counts, binary flag, and hunks — replacing the previous "list via `get_git_status`, then one `diff_file_against_head` round-trip per file click" pattern. This is the shape the stacked review surface consumes.
3. **git2 enumerates deltas; the existing pipeline renders them.** Rename and binary detection come from the `git2` diff; each text delta's old (merge-base blob) / new (workdir) content still flows through `compute_file_diff` → `group_into_hunks` → `build_hunk`, preserving syntect highlighting and bounded (`-U3`) context.
4. **Fallback chain keeps non-`origin` repos working.** If `base_ref` is unresolvable (no `origin`, ref deleted), fall back to `HEAD`. If `merge_base` fails (unrelated histories), fall back to the `base_ref` commit directly, then to `HEAD`. A repo with no commits yet diffs against the empty tree (whole working dir as additions), exactly as before.

## Considered alternatives

- **Keep diffing against `HEAD`.** Rejected: invisibly loses committed work from the review view the instant an agent commits — the failure mode that motivated this ADR.
- **Diff against `base_ref` directly (skip the merge-base).** Rejected: if the base branch has advanced since the worktree was cut, the diff smears the agent's work together with unrelated upstream commits the agent never touched. The merge-base is the last common ancestor, so only the agent's own divergence shows.
- **Two diffs — committed (`merge-base..HEAD`) shown separately from uncommitted (`HEAD..workdir`).** Rejected for now: closer to a true PR review, but doubles the UI and the mental load. A single "since you started" diff matches the cornerstone use (glance at what an agent did) and can be split later if the demand appears.

## Consequences

- **The cornerstone view now matches intent.** "Changed Files" answers "what did this agent do since branching," committed or not — stable across the agent committing its work.
- **Semantics shift for every consumer.** The desktop panel, the mobile `/api` diff route, and the per-file summary counts all move to merge-base together (no mixed meaning across surfaces). The checkpoint diff (`diff_session_checkpoint`) is unaffected — it intentionally diffs against a specific checkpoint ref, not the base.
- **`base_ref` is now load-bearing for diffs, not just for worktree creation and PRs.** A mesh pointed at the wrong base will mis-scope its diffs; this makes the existing base_ref setting more visible in its effect, which is desirable.
- **One round-trip instead of N.** Opening the panel fetches the entire change set once; clicking a file is now a scroll, not a backend call.
