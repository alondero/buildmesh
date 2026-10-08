# 43. Actionable blocked worktree cleanup with backoff and suppressed repeats

Status: accepted (issue #2139; partially supersedes [ADR-0004](0004-optimistic-node-close-deferred-worktree-removal.md)'s failure surfacing)

A worktree removal that cannot complete is persisted as a **blocked cleanup** with
its evidence, backed off instead of retried in a loop, and reported to the user
**once per unchanged blocker** — from a dialog that offers copy, diagnosis, retry
and "keep worktree". ADR-0004's core decision (the durable
`pending_worktree_removals` queue, the three phases, dequeue-only-on-success)
still stands; what this record changes is what happens on the *failure* branch.

## Context

ADR-0004 step 4 promised that "a worktree that genuinely can't be removed raises
a `worktree-cleanup-failed` toast". Two properties of that promise made it
useless in the field:

1. **The toast hid everything actionable.** The event carried `node_name`,
   `worktree_path` and `error`, and the listener rendered only the node name plus
   "it'll be retried on next launch". The user could not see which directory was
   stuck, why it was stuck, or which step failed — and there was no action beyond
   waiting for a restart.
2. **Every drain re-warned.** Closing one Agent Node drains the whole queue, so a
   machine with eight blocked cleanups produced eight toasts, and the next close
   or the next launch produced eight more. The removal path already retries a
   rename five times with a 100 ms backoff (`remove_worktree_dir_with_retry`),
   but the *drain* itself has no backoff: it re-attempted, and re-warned, on
   every pass.

A Windows instance reviewed on 2026-10-08 had eight blocked cleanups. Handle
inspection found four PowerShell sleepers holding child directories, two external
recycle-bin helpers holding worktree roots, and Explorer holding folders in two
other worktrees. An isolated fixture reproduced error 32 for a root handle and
error 5 for a child-directory handle, **including a handle that permits delete
sharing** — so closing a child folder can clear an apparently
permissions-related error. That is only discoverable if the user is told which
folder and which permission failed, and can see which process to look at.

A third defect sat underneath both: a removal interrupted *between* the rename
into `<path>.removing` staging and the staged delete left the staging directory
and a live git admin entry, while `remove_worktree_inner`'s
"missing directory = already removed" short-circuit dequeued the tombstone —
leaking a directory with no error, no retry and no toast (the #1080 shape).

## Decision

**1. The queue row carries the failure.** `pending_worktree_removals` (schema
v47) gains `attempt_count`, `last_attempt_at`, `last_operation`, `last_error`,
`retry_not_before` and `notified_error`. `operation` is drawn from a stable
vocabulary (`rename-worktree-to-staging`, `delete-staged-worktree`,
`prune-worktree-admin-entry`, …) rather than an ad-hoc string, because
"rename blocked by another process" and "delete blocked by another process" are
different recoveries, and the identifier must survive a UI rewrite.

**2. The removal reports which step failed.** `git::worktree` gained
`remove_one_worktree_and_branch_detailed`, returning
`Result<(), WorktreeRemovalFailure>` (`operation` + the unchanged OS error text).
The `Result<(), String>` callers are unchanged.

**3. An absent path is not an absent task.** When the working directory is gone,
the removal still reclaims a leftover `.removing` staging sibling *and* prunes
the orphaned git admin entry (found by opening the repository from above and
matching the entry's `gitdir` file to the path). Failure to finish either half is
an error, so the queue entry survives until the disk and the git bookkeeping are
both done.

**4. The drain backs off and de-duplicates.** `process_pending_removals` skips a
row inside its `retry_not_before` window (5 s doubling to 5 min), records each
failure, and asks for an event only when the blocker's signature
(`operation: error`) differs from the one already reported in `notified_error`.
An unchanged blocker therefore warns once, not once per drain — while the queue
keeps the evidence for whenever the user looks.

**5. The failure surface is a dialog, not a sentence.** The
`worktree-cleanup-failed` event carries node identity, full path, failed
operation, error, attempt count, last attempt and next retry; `App.tsx` opens a
dialog that lists **every** blocked cleanup from
`list_pending_worktree_removals` (the queue is the source of truth, not the
event) and offers Copy path, Copy diagnostics, What is holding it?, Retry and
Keep worktree per entry. There is deliberately no second, persistent entry point
to reopen the dialog once dismissed: the criterion is that the *failure* is
actionable when it appears, and a permanent indicator would compete with the
toast stack on every boot. A Probe entry for "blocked cleanups" is the obvious
follow-up if dismissing it turns out to be too easy.

**6. Process diagnosis is explicit, read-only and approximate.**
`worktree_blockers::diagnose_blocking_processes` reports processes whose
executable or working directory lies inside the tree — on Windows the toolhelp
snapshot plus the target's process environment block (WOW64 bitness handled), on
Unix `/proc/<pid>/{cwd,exe}`. It names *candidates*; the OS error says which
permission failed. Sysinternals `handle.exe` would name the holder of a specific
handle, but it cannot be assumed installed.

**7. Nothing is terminated automatically.** `release_worktree_cleanup_blocker`
is reachable only from a user clicking "End process" on a diagnosed row, one pid
at a time. The removal path never kills anything.

## Alternatives considered

- **A retry button in the toast.** Rejected: toasts cap at three and expire in 15
  s, so with eight blocked worktrees only some would ever be actionable. A dialog
  that lists the queue is the only shape that scales to the reported instance.
- **Re-notify every drain, but dedupe in the UI.** Rejected: the suppression has
  to be durable to survive a restart, and the UI cannot know whether an unchanged
  blocker *was* already reported.
- **Kill the diagnosed processes ourselves.** Rejected: the diagnosis is
  heuristic, and the holders in the reported instance were external applications
  (recycle-bin helpers, Explorer). Terminating them without being asked is
  exactly the surprise Buildmesh should never spring on a user.
- **Ship `handle.exe` with the app.** Rejected: a licensed third-party binary
  for a diagnostic the heuristic covers well enough, and it names a handle rather
  than the recovery.
- **Infer blocked cleanups from `git worktree list`.** Rejected: inference is
  what ADR-0004 already rejected; the queue records intent, and the failure
  columns hang off the same recorded intent.

## Consequences

- **Failure is a first-class state.** The queue, the event and the dialog all
  read the same evidence, so "eight blocked cleanups" is a list, not a sentence.
- **Idle retry pressure drops sharply.** A permanently blocked worktree costs one
  removal attempt per backoff window (max every 5 minutes) instead of one per
  drain, and one user-visible warning instead of one per drain.
- **A removed directory counts as removed, and an absent one is finished
  properly.** Interrupted removals no longer leak `.removing` staging siblings or
  orphaned admin entries, and a tombstone is only retired when both halves are
  done.
- **A blocked cleanup can be abandoned.** "Keep worktree" dequeues the intent;
  nothing on disk changes, and the directory stays visible to the user.
- **The schema grows six columns** (v47, additive with defaults). Pre-v47 rows
  read as "never attempted, nothing failed, not notified".
- **Test-suite discipline.** The pinning fixtures used by the Windows removal
  tests now own their children through a `ScopedChild` guard that kills *and
  reaps* on drop, so a failing assertion can no longer leave a directory-pinning
  process behind — the audit item #2139 asked for. Without it, a red test run
  leaks exactly the sleeper processes the issue is about.

## Verification

See `docs/development/agent-nodes.md` (*Close/removal*) for the runtime narrative.
Backend: `git/worktree` staging-recovery and failure-operation tests; `db/tests`
failure-bookkeeping and notification-signature tests; `services::agent_node`
backoff, suppression and retry tests; `worktree_blockers` path-membership and
live-process diagnosis tests. Frontend: `tests/unit/worktree-cleanup-diagnostics`,
`tests/unit/blocked-cleanup-store`, `tests/unit/blocked-cleanup-dialog`.
