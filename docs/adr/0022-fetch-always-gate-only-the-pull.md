# 22. Fetch Always, Gate Only the Pull — and Pin the Refspec

Every mesh sync path (spawn-time auto-sync, background worker, manual Sync
button, PR-head fetch) now runs the `git fetch` unconditionally and applies the
dirty-working-tree gate only to the `git pull --ff-only` step. The fetch also
always passes an explicit refspec so the remote-tracking refs are guaranteed to
advance, and the behind-count falls back to the branch's `.git/config` tracking
entries when git2's refspec-based upstream mapping fails.

## Context

Two production incidents on 2026-07-17 exposed compounding freshness holes:

1. **The dirty gate sat before the fetch.** ADR 0001's "skip if dirty"
   criterion was implemented one step too early: `do_fetch_only` bailed on a
   dirty checkout before the network round-trip. A `git fetch` never touches
   the working tree — only the fast-forward pull does — so the gate's data
   safety rationale only ever applied to the pull. Because worktree Agent
   Nodes are cut from the *remote-tracking ref* (`refs/remotes/<r>/<b>`), a
   mesh whose root checkout stayed dirty (normal when the user orchestrates
   from it) never freshened that ref on **any** path: background sync, spawn
   auto-sync, manual Sync click, and PR-head fetch all skipped. Nodes went
   stale without bound, while diagnostics recorded thousands of "successful"
   background syncs completing in 7–26 ms (skips counted as attempts).

2. **A URL-only remote defeated both the fetch and the count.** A repo whose
   `.git/config` had `remote.origin.url` but no `remote.origin.fetch` refspec
   (remote wired by hand rather than `git remote add` — seen on pixelcache):
   - `git fetch origin [<branch>]` stored the result only in `FETCH_HEAD`;
     with no refspec there is nowhere to write the tracking ref, so
     `refs/remotes/origin/main` stayed frozen at a five-day-old SHA.
   - git2's `branch_upstream_name` maps `refs/heads/<b>` through that same
     refspec, so `commits_behind_upstream` errored — and `do_sync` swallowed
     the error into `UpToDate`. The Sync button reported "Already up to date"
     while a terminal `git pull` (which reads `branch.<b>.merge` directly)
     pulled the whole backlog.

## Decision

1. **Move the dirty gate from `do_fetch_only` to `do_sync` Step 5** (after the
   fetch and behind-count, immediately before the pull). `SkippedDirty` is
   replaced by `FetchedButDirty { new_commits }`: the fetch reached the remote
   and the tracking refs are current; only the checkout's fast-forward was
   skipped. It counts as `fetched_ok` (stamps the ADR 0020 freshness TTL) and
   as `advanced_ref` (triggers the warm-pool refresh pass), because the refs
   worktrees are cut from did move.
2. **Always pass an explicit refspec to `git fetch`**:
   `+refs/heads/<b>:refs/remotes/<r>/<b>` for a narrowed fetch, or the
   `+refs/heads/*:refs/remotes/<r>/*` glob when fetching all refs from a
   remote whose `remote.<r>.fetch` config is missing. (A remote with its own
   configured refspecs keeps them for the all-refs case — it may deliberately
   narrow or remap.) The leading `+` mirrors the default refspec's force
   marker and doubles as an argv-injection guard.
3. **Fall back to `branch.<b>.remote`/`.merge` config** in
   `upstream_oid_for_branch` (and `current_branch_upstream_remote`) when
   `branch_upstream_name` fails — the same lookup `git pull` itself uses.
   This makes the behind-count (and the sidebar ↓N badge) truthful on
   URL-only remotes.

## Consequences

- **Pros:** After a manual Sync reports success, every Agent Node created
  afterwards is cut from refs at least as fresh as that sync — the user's
  core confidence contract. Dirty meshes stay continuously fresh in the
  background. The PR-head fetch works on dirty meshes. `git status`-clean
  repos behave exactly as before.
- **Cons:** A spawn against a dirty mesh now pays the fetch it used to skip
  (bounded by the ADR 0020 freshness TTL, so in steady state the background
  worker has already paid it). A dirty mesh's manual Sync reports
  "Fetched N new commits; fast-forward skipped: working tree has uncommitted
  changes" instead of a plain skip — slightly noisier, deliberately honest.
- **Unchanged:** The pull is still never attempted on a dirty tree (ADR
  0001's actual data-safety concern), still `--ff-only --no-rebase`, and the
  sync still never blocks a spawn.
