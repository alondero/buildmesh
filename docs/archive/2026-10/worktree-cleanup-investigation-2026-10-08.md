# Worktree cleanup investigation: blocked removals and repeated warnings

Maintainers can use this investigation to prioritize actionable cleanup recovery
and distinguish Windows folder locks from permission problems.

Status: historical, point-in-time investigation on 2026-10-08. Recommendations
are proposed behavior, not a current product contract. Follow-up implementation
is tracked in [issue #2139](https://github.com/alondero/buildmesh/issues/2139).

Source baseline: `71c6ab55d3d9b5fa37308f07e3a60a6e12dfb689`. The running Windows
release reported commit `99e4cfc60`; the cleanup modules examined were unchanged
between that commit and the source baseline. Observation time: approximately
12:15 UTC. Personal paths, process identifiers and raw logs are omitted.

## Observed blockers

The live database contained eight pending Worktree removals, queued between
2026-10-06 and 2026-10-08. All eight directories still existed, remained
registered with Git, and had no matching Agent Node row. None had a sibling
`.removing` directory.

Read-only handle enumeration with [Microsoft Sysinternals
Handle](https://learn.microsoft.com/en-us/sysinternals/downloads/handle) found
open folders matching every queued path:

| Count | Handle holder | Held location | Logged cleanup error |
|---|---|---|---|
| 4 | PowerShell with `Start-Sleep -Seconds 600` | Worktree child `src-tauri` | Access denied, OS error 5 |
| 2 | PowerShell external recycle-bin helpers | Worktree root | Sharing violation, OS error 32 |
| 1 | Explorer | Worktree child `docs/prototypes` | Access denied, OS error 5 |
| 1 | Explorer | Worktree child `tools` in another Mesh | Access denied, OS error 5 |

The six PowerShell helpers' parent processes were no longer running. The sleeper
command matches the repository's
[retirement crash fixture](../../../src-tauri/src/services/legacy_retirement/retirement_crash_tests.rs).
The recycle-bin helpers referenced individual scratch files and were external to
Buildmesh's cleanup implementation. Process ancestry alone does not establish
why these helpers survived, whether a particular Job Object failed, or which
test invocation created each sleeper.

Normal permissions allowed opening the six error-5 roots with delete access.
Directory scans found sharing conflicts in the four held `src-tauri` directories.
The two error-32 roots rejected the same read-only access probe. Explorer's held
child folders allowed delete access, but the isolated reproduction below shows
that delete sharing does not guarantee an ancestor can be renamed.

The evidence strongly supports open directory handles as the current blockers.
No live Worktree was renamed or deleted, and no owning process was stopped, so
successful removal after releasing each live handle remains unverified.

## Windows reproduction

An isolated, empty fixture under the investigating Worktree reproduced the
removal algorithm's rename step. `CreateFileW` opened a directory with
`FILE_LIST_DIRECTORY` and `FILE_FLAG_BACKUP_SEMANTICS`; the probe then tried to
rename its root to a sibling staging name.

| Held directory | Sharing flags | Root rename result |
|---|---|---|
| Root | Read and write | OS error 32 |
| Child | Read and write | OS error 5 |
| Child | Read, write and delete | OS error 5 |
| No remaining handle | None | Success |

Closing each handle restored successful rename. The empty probe was removed.
These executed cases explain why "Access denied" can mean an open child folder
in this instance. They do not establish that every error 5 has the same cause.
No application test suite or production cleanup was executed during the initial
diagnostic review.

## Why warnings repeat

The inspected source has this flow:

```text
delete_agent_node
  drain_pending_removals
    process_pending_removals: read the entire durable queue
      failed remove: retain the row and return the failure
    emit worktree-cleanup-failed for each failure
      App: toast with node name and next-launch retry message
```

Every successful node deletion invokes
[the drain](../../../src-tauri/src/commands/agent_node.rs), including deletions
that do not request Worktree removal. Startup also invokes it. The
[service](../../../src-tauri/src/services/agent_node.rs) serializes drains, but a
later drain retries every retained failure again. Five rename attempts with four
100 ms waits absorb brief handle-release delays; they cannot resolve a folder
held open for hours or days.

The event already contains the node name, Worktree path and error. The
[App listener](../../../src/App.tsx) discards the path and error when rendering
the warning. [Toast deduplication](../../../src/lib/toastUtils.ts) lasts 15 seconds
and caps the visible stack at three, so eight recurring failures can replace one
another and recur after dismissal.

The [queue schema](../../../src-tauri/src/db/mod.rs) stores path, node name and
creation time, but no last error, attempted operation, retry count, next attempt
or notification state. There is no durable failure list or dedicated retry
action. Automatic draining occurs on deletion and startup, not on a periodic
cleanup schedule. Restarting Buildmesh cannot release folders owned by unrelated
Explorer or helper processes.

## Recovery available at the inspected baseline

For Windows users with these symptoms:

1. Locate the directory in Repository's **Branches and worktrees** list. Node
   names can differ from directory names after a rename; use the full path.
2. Download Microsoft Sysinternals Handle from the linked official page. From
   its extracted directory, run the command below with the affected directory's
   actual name. Handle visibility depends on process permissions; an empty
   result does not prove there is no lock.

   ```powershell
   & .\handle64.exe -nobanner 'worktree-directory-name'
   ```

3. Close the matching Explorer tab/window or move the owning terminal out of
   the held folder. For an abandoned helper, verify its current command and
   creation time in Task Manager's **Details** view before ending that process.
   Process identifiers can be reused after a process exits.
4. Retry removal through Repository, or restart Buildmesh after releasing the
   handles. Repository deletion uses the same removal primitive, so it cannot
   bypass a remaining lock. That path removes the Worktree; branches are managed
   separately there. It does not immediately reconcile the pending queue row,
   which is cleared by a subsequent drain.

The original reporter received the exact local paths and process identifiers
privately in the diagnostic response. General recovery belongs in the current
[troubleshooting guide](../../troubleshooting.md); this report records the
baseline's limitations rather than promising new controls.

## Recommended implementation

### Persistent failure entries and explicit actions

Expose queued cleanups in Repository, with a visible blocked count. For each
entry, show node identity, full host path, last attempt, failed operation and
error. Provide **Copy path**, **Copy diagnostics**, **Retry**, and **Keep worktree
/ cancel cleanup**. Fetch entries on startup as well as observing events, so
listener timing cannot hide an existing failure.

Retry should target the selected queued entry and reconcile its durable status.
Keep worktree should cancel removal intent without pretending disk reclamation
succeeded. Revalidate active ownership before removing a path that may have
been reused; a historical failure must not authorize deletion of a live node's
replacement Worktree.

### Quiet retries and process diagnosis

Persist bounded retry metadata and next-attempt time. Schedule new cleanups
promptly, then back off unchanged failures independently of unrelated node
closes. Emit a notification when cleanup first becomes blocked or its condition
materially changes; update persistent entries quietly on repeated attempts. A
notification should link to **View cleanup issues**.

Offer handle diagnosis on demand, showing the owning application, PID, creation
time and exact held folder. Recheck that identity before any explicit **Stop
helper and retry** action. External applications require user intervention;
automatic cleanup must not terminate Explorer or unrelated work. Diagnostics
failure should retain the cleanup entry and offer the path and raw OS error.
Choose the diagnostic implementation with directory locks in mind:
[Restart Manager's RmGetList](https://learn.microsoft.com/en-us/windows/win32/api/restartmanager/nf-restartmanager-rmgetlist)
returns access denied for registered directory resources, so using it alone
would miss this instance's central failure pattern.

### Prevent helper leaks

Audit test/helper spawn ownership, failure paths and working directories. The
retirement fixture currently inherits the test process's working directory and
forgets the sleeper child before confirming Job Object containment. Prefer a
fixture-owned temporary working directory and retain a cleanup guard until
containment is confirmed, including assertion and timeout paths. The fixture's
command match is a lead for further diagnosis, not proof of the specific escape
mechanism in these four processes.

### Complete interrupted staged removal

A separate static defect exists in
[the removal implementation](../../../src-tauri/src/git/worktree/mod.rs): after
the original path is renamed to `.removing`, a failed staged deletion leaves
the original absent. The next attempt returns success at the original-path
existence check before inspecting staging, so the queue can be cleared with disk
contents and Git bookkeeping still outstanding. Clearing old staging also
currently ignores errors. This condition was absent from the eight live entries
and was not executed through the production removal function in this review.

Preserve enough durable context to recover staging, prune Git metadata and
complete branch cleanup across a restart. Dequeue only after all required
cleanup phases finish; attach operation and path to failures.

## Acceptance evidence for follow-up work

- Repeated unrelated node closes retry eligible entries without repeated
  notifications for an unchanged blocker; retry state survives app restart.
- The UI exposes existing failures even when startup events precede listener
  registration, and shows success or failure after a targeted retry.
- A held root and held child directory reproduce errors 32 and 5 respectively
  at the production removal boundary, preserving cleanup intent in both cases.
- A crash or deletion failure after staging rename retains the entry; the next
  attempt reclaims staging and completes Git bookkeeping before dequeueing.
- Cancelling cleanup preserves the directory. A reused path belonging to a live
  node is protected. Explicit process intervention rejects a stale PID identity.
- Helper containment failure and parent termination leave no fixture processes
  or handles in the checkout.

See the current [Agent Node lifecycle documentation](../../development/agent-nodes.md)
and [optimistic-close ADR](../../adr/0004-optimistic-node-close-deferred-worktree-removal.md)
for the existing lifecycle boundary. Implementation needs its own behavior
tests, current recovery documentation and real Windows UI evidence.
