# 33. Fast-forward pull is blocked only by local changes it would overwrite

The mesh sync's pull gate is no longer "is the working tree dirty?". It is now
the precise set of paths the `git pull --ff-only` would rewrite that also carry
a local change — exactly the condition git itself refuses on. Local edits and
untracked files in paths the incoming commits do **not** touch no longer block
the fast-forward. `FetchedButDirty` carries the blocking paths and its
user-facing message names them.

## Context

ADR 0022 moved the dirty gate from before the fetch to before the pull, but the
gate itself stayed coarse: `do_sync` Step 5 skipped the fast-forward whenever
*any* non-ignored change existed (modified, staged, deleted, **or untracked**).
That is stricter than git. `git pull --ff-only` is a fast-forward checkout: it
does not care that the tree is dirty in general, only that no path it is about
to rewrite has a local modification it would clobber. Concretely
(reproduced 2026-09):

- Local edits to `b.txt` and an untracked `scratch.txt`, upstream ahead by a
  commit that touches only `a.txt` → `git pull --ff-only` **succeeds**, moving
  the branch and leaving both local changes intact.
- Local edit to `a.txt`, which the incoming commit also rewrites →
  `git pull --ff-only` **aborts** with `error: Your local changes to the
  following files would be overwritten by merge: a.txt … Aborting`, leaving the
  tree untouched.

The coarse gate therefore blocked the common case. The reported symptom: a mesh
(aerogauge-recomp) showed 3 commits to pull but Buildmesh refused, because the
mesh root had uncommitted or untracked changes in files the incoming commits
never touched. Untracked files were the worst offender — build artifacts, env
files, and scratch output are normal in a mesh root and can essentially never
conflict with a fast-forward unless the incoming commits add the same path.
The result was unbounded "behind" badges and users stashing or committing
unrelated work just to let the auto-sync run.

## Decision

1. **Replace the boolean dirty gate with a path-overlap predicate.**
   `git::primitives::ff_pull_blockers(repo)` returns the paths that a
   fast-forward would overwrite — the intersection of
   - the locally dirty/untracked paths (the submodule-ignore-aware path set
     that `is_dirty_mirrors_git_status` used to fold to a bool, now exposed as
     `dirty_paths_mirrors_git_status`), and
   - every path the `HEAD..upstream` diff touches.

   Both sides of each diff delta are included, so a local edit at the source of
   an upstream rename/delete and an untracked file at the destination are both
   caught. `Ok(vec![])` means the fast-forward is safe despite a dirty tree.

2. **`do_sync` Step 5 proceeds to the pull when the set is empty**, letting git
   perform the fast-forward and preserve the untouched local changes. It skips
   only when the set is non-empty, returning
   `FetchedButDirty { new_commits, blocking_paths }`. Unreadable status/diff
   fails closed (skip), as before.

3. **Carry the paths through to the UI.** Both `SyncOutcome::FetchedButDirty`
   and `FetchOutcome::FetchedButDirty` gained `blocking_paths: Vec<String>`.
   The manual Sync message becomes "Fetched N new commits; fast-forward
   skipped: changes to `<paths>` would be overwritten" (capped at three names),
   replacing the misleading blanket "working tree has uncommitted changes".
   The spawn path stays silent (it logs the paths at `info`) because the new
   node is still cut from the freshly-fetched refs.

4. **Delete the now-unused `is_dirty_mirrors_git_status` wrapper.** Its path-set
   core (`dirty_paths_mirrors_git_status`) is the single source, consumed only
   by `ff_pull_blockers`. The strict `is_dirty` remains the gate for
   data-safety decisions (worktree close, base restore).

## Consequences

- **Pros:** Mesh roots with untracked files or unrelated edits stay fresh — the
  auto-sync pulls the backlog instead of freezing the behind-count. The pull is
  still never allowed to clobber uncommitted work: when it is skipped, the user
  is told *which* files block it, so the message is actionable rather than a
  blanket accusation. The extra work is a `statuses()` call plus a
  `diff_tree_to_tree` of the changed paths — both local, no network.
- **Cons:** `FetchedButDirty` and its message now carry a path list, so the
  enum shape changed (both internal). A fail-closed block (unreadable status)
  still reports the generic "uncommitted changes" wording with an empty path
  list. A rare case where the local file's content already equals the incoming
  target is still treated as a blocker and the pointer stays behind; git would
  have accepted it, but the paths are path-overlapping and the next sync
  resolves it once the content diverges or the user commits.
- **Unchanged:** The fetch always runs (ADR 0022). The pull is still
  `--ff-only --no-rebase`. The sync still never blocks a spawn. Divergence and
  fetch failures keep their existing outcomes.
