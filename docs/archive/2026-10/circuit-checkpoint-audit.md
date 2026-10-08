# Circuit checkpoint audit, 8 October 2026

Historical investigation, not the current contract. See [Circuit architecture](../../development/circuits.md).

## Live inventory

Read-only SQLite queries against the running production profile at 13:43 UTC found
five running runs, 265 failed runs, 90 completed runs and 12 cancelled runs.
All failed runs had no remaining live source or step agent (joined agent status
excluding archived/lost). They are retained history, not currently executing work.

| Run | Checkpoint | Observed evidence | Diagnosis |
| --- | --- | --- | --- |
| 382 | `await_publish`, Unverified | Codex Ready; result file ends in `BUILDMESH_HANDOFF_V1: READY`; repeated completed classifications rejected | Result text was substituted without rebinding the report |
| 383 | `await_publish`, Unverified | Command Code Ready; full result differs from terminal summary; repeated completed classifications rejected | Same result/report binding mismatch |
| 384 | `await_publish`, Unverified | Earlier completed classification rejected; result file exists; Codex resumed at 13:40 UTC and has no current finished turn | Same earlier mismatch, followed by a legitimate wait for the new turn; the old result cannot bypass current-turn admission |
| 385 | `reviewer`, Unverified | Command Code Ready; result ends in `BUILDMESH_REVIEW_V1: REQUEST_CHANGES`; completed report classification rejected | Same result/report binding mismatch; successful handoff must still route through the review verdict |
| 386 | `await_source`, Running | This investigation's source is actively working | Expected wait, not a stalled checkpoint |

The binding repair originated in `unseen-tedious-diamond`, commit
`cc1c4f4776523bec33cf8bedfeb22711ca0f1f93` (PR #2145, issue #2143), and is
included here. A result is bound to the admitted session/input/turn, with both
result and transcript freshness retained. It does not establish native lifecycle
completion or approval. Current-turn and human/owned-work blockers still apply.

## Historical failures

The failed ledger was inventoried across all 265 runs, including cancelled steps
inside failed runs. Recent examples: 379/380 lost their source after long-paste
feedback delivery failed; 381 lost its source while waiting for a usable report;
374 lost its reviewer after a missing publication timestamp; 371 lost its source
while session identity was unavailable; 368 lost its source after classifier
failure. Earlier groups include exhausted review retries, unavailable classifier
or report/session evidence, agent errors, unavailable processes, and unacknowledged
prompt delivery. Run 362 failed the explicit unpushed-branch verification.
The long-paste path is already replaced by prompt files in base commit `99e4cfc6`.
No historical run was replayed, attested complete, or changed in the live database.

## Additional result-read failure

The imported repair still mapped every result-file I/O error to "missing". That
can spend a bounded reminder on an unreadable file or a result changing during
inspection. Only a successfully observed missing/blank file should use reminders.
Read errors defer observation with an actionable diagnostic and retry through the
existing probe schedule, without classifying the transcript or sending input.

The test uses an unreadable directory at the expected file path for a deterministic
cross-platform error, then replaces it with a valid result to prove recovery.
Separate report freshness tests cover transcript changes and result replacement.
These tests exercise report admission, the production stepper and commit fences;
they do not claim the running production binary has been upgraded.
