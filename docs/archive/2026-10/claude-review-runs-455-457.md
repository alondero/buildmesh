# Claude Code review runs 455 and 457

Historical investigation on 12 October 2026. Source comparison base:
`d59bd6b93a794eff044039cb921f18461ba5cb34`. The stable Windows app was
inspected through read-only SQLite queries, its log, and Claude Code 2.1.296
transcripts. The investigation did not change either run or restart the app.

## Run 457: a stopped task remained pending

Run 457, agent 5354, remained Running at `await_source` with the persisted
`known_work_outstanding` blocker. The last lifecycle report was
`background_running`, from an idle Notification at 19:52:00 UTC.

The transcript establishes this sequence:

| UTC | Native evidence |
|---|---|
| 19:22:26 | Vite launched as background task `bziuux8nz`. |
| 19:26:41 | A correlated `TaskStop` result confirmed that `bziuux8nz` was successfully stopped. No terminal task notification for that ID was recorded. |
| 19:31:51 | CI watcher launched as background task `bx4tseh2w`. |
| Before final report | A completed task notification settled `bx4tseh2w`. |
| 19:51:00 | Stop reported an empty native active-work registry and the final implementation report. |

The attention route's transcript scan recognized task notifications but ignored
successful `TaskStop` results. It therefore counted `bziuux8nz` as pending and
published BackgroundRunning. That negative lifecycle report vetoed Circuit
report admission even though the native Stop registry was empty.

The reader change recognizes a successful TaskStop result only when its tool
call and task ID match. The sanitized on-disk fixture preserves the native
launch, stop and completion shapes. Before the fix, the real attention
normalizer regression returned SuppressPendingBackground after the successful
stop instead of Ready. The corrected scanner replayed the full captured
transcript with zero pending tasks. The normalizer regression covers both Stop
and idle Notification callbacks, including a separate task that remains active.

This removes a false lifecycle veto; it does not grant workflow approval.
The final report also asks whether to apply a live database migration after
deployment, and the stable app's classifier logged BLOCKED. A deployment
decision can remain after the stopped-task defect is corrected. Live advancement
was not tested because the stable process still contains the original reader.

## Run 455: session continuation callbacks were rejected

Run 455, agent 5350, had two distinct phases:

1. Claude's Stop callbacks reported `task:bu6lva8b5` as genuinely outstanding.
   This was a helper Bash loop waiting for test output. Its completion
   notification did not appear until 20:06:21 UTC in the successor transcript.
2. At 18:57:48 UTC the original transcript recorded `continued-in`, mapping
   session `d1b48df8-eb97-4867-b365-a34cdbdc5900` to
   `a5acfe57-a409-4aaa-a644-4f7621934663`. Buildmesh retained the old session ID.
   From 18:58:08 UTC onward its log repeatedly rejected the successor's hooks
   as stale callbacks from a previous process.

An operator attestation completed `await_source` at 19:18:18 UTC. The run
injected its publish request, then waited at `await_publish` until cancellation
at 19:23:47 UTC. The implementation agent was subsequently deleted. The source
report and completion hooks in the continued session could not update the run
through the stale-session fence.

This continuation defect needs a separate identity-reconciliation change:
prove the native old-to-new session lineage before updating the stored ID, and
retain rejection of unrelated or retired-process callbacks. Merely allowing any
different hook session ID would weaken the existing fence. The stopped-task
reader change does not address that transition. The captured live lineage and
32 rejected successor hooks reproduce the failure; an automated regression at
the identity-reconciliation boundary is deferred to
[issue #2263](https://github.com/alondero/buildmesh/issues/2263).

## Evidence location

Raw run contexts, history and the full run-457 transcript are ignored local
evidence in `.tmp/claude-circuit-investigation/`. They contain agent content and
are not published. The committed fixture contains only sanitized task records.
The current attention contract is in
[attention.md](../../development/attention.md); this document records the
historical incident, not the current implementation contract.
