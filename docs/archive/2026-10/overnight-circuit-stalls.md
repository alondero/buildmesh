# Overnight circuit stall audit

Historical developer investigation, 2026-10-13, against base
`fb5b458a9137a66ef8aef11766247f3938cbf828`. Current contracts are in
[Circuits](../../development/circuits.md) and
[attention](../../development/attention.md).

Read-only inspection of the stable SQLite ledger found seven overnight active
runs on mesh 65, circuit 5. All seven implementers were MiniMax Code. Evidence
included graph snapshots, persisted step and observation state, result files,
canonical MiniMax transcripts and background-task stores. The ledger capture
was taken around 05:47 UTC. Several sessions had already received operator
continuations around 05:35-05:45 UTC; their new work must be distinguished from
the overnight stall. No stable database mutation or agent prompt was made by
this investigation.

## Runs and obstacles

| Run | Issue / node | Evidence and disposition |
|---|---|---|
| 460 | 2254 / 5373 | Implementation report explicitly said saturated-load acceptance was not met and requested a maintainer decision about verification coordination and load limits. `implementation_classifier` recorded BLOCKED. This is a genuine task blocker, not permission to advance the gate. |
| 463 | 2016 / 5376 | PR 2268 received changes requested for cancellation races. Fixes were pushed, but the feedback result file said fixes were done without the required handoff decision line. Inference returned BLOCKED and disabled the ordinary evidence deadline. Verification also reported an unlisted browser capture flake. Ask for a valid phase result through bounded format repair; retain the independent review requirement. |
| 468 | 2264 / 5384 | Canonical final report at 02:35:39 UTC declared READY, with all eight launched background tasks terminal. Persisted Stop lifecycle at 02:35:56 still said BackgroundRunning, so the implementer stayed Unverified with `known_work_outstanding`. MiniMax hooks were using the Claude scanner over a temporary compatible transcript; that temporary file was already absent during inspection. The old scan's exact false positive cannot be reconstructed, but applying the wrong harness scanner is demonstrable. Use native canonical task evidence for subsequent Stop reconciliation. |
| 469 | 2255 / 5387 | PR 2275 had independent approval. The merge report described green checks followed by an advanced main branch; updating the branch created a new head and Actions run 38353099083. It then waited without another assistant turn for roughly two hours. Preserve fresh-head CI and merge checks; waking a finished task cannot itself establish merge success. |
| 470 | 2240 / 5389 | Background completion notifications followed the old implementation report without another assistant turn. The finish gate became Unverified because newer input/tool activity superseded its report; Unverified steps were excluded from wake observation. After operator continuation, verification encountered Vite dependency-cache 504 responses and resource contention. |
| 471 | 1704 / 5392 | A completed-task runtime notification was followed by about two hours without an assistant turn. The finish gate was Unverified and excluded from wake observation. At capture, one launched task still lacked terminal transcript evidence; unknown task state must remain a recovery veto. Resumed verification also hit Vite cache failures. |
| 472 | 1426 / 5393 | A long test command returned `details.status = auto_promoted` at 03:28:56 UTC. Its runtime completion notification arrived at 03:31:36, with no subsequent assistant until the operator continuation around 05:45. The detector admitted only `started`, so it missed this owned task entirely; stale Running status and an Unverified step provided two further exclusions. |

## Repairs and safety boundaries

At a later read around 06:32 UTC, runs 469 and 471 were Completed, run 470
had reached review (PR 2277), and run 472 had reached an Unverified reviewer
(PR 2278). Runs 460, 463 and 468 retained their original checkpoints. These
changes preceded deployment of this patch and followed operator continuations;
they are not evidence that this source change repaired the stable runtime.

The native MiniMax reader recognizes timeout promotion as a launch, reconciles
terminal tool receipts and task summaries, and refuses to infer task completion
from output-file silence. Every owned launch needs terminal evidence before a
wake. Pending foreground tool calls also prevent a wake.

Circuit wait observation now considers Unverified steps without advancing their
ordinary wait budgets. Claiming their single wake returns the step to Running,
allowing its delivered/obsolete/uncertain outcome to settle durably. A quiet
Running node can be considered only with native finished-task evidence;
dispatch rechecks task identity and all existing session/input fences. Genuine
questions, permissions, borrowed source agents and ongoing output remain fenced.

Explicit-result phases now repair absent or malformed decision lines before
inference, using the existing two-reminder limit. This preserves READY versus
BLOCKED and independent review semantics rather than guessing from prose.

The active session verification failures are separate obstacles. Shared Vite
dependency-cache invalidation and Windows browser validation contention are
already tracked by issue 2029; unlisted browser capture flakes require their
actual failing-test evidence rather than being waived as a circuit success.

## Verification and deployment boundary

Before production changes, three new regressions failed: timeout-promoted task
ownership, Unverified wake observation, and a nonempty result missing its
required decision. Additional coverage checks terminal versus unknown task
state, concurrent foreground work, native Stop reconciliation, and stale wake
delivery fences. Canonical verification and independent review are recorded in
the pull request and worktree harness receipt.

The stable application and its open nodes were not restarted. These source
repairs require a rebuilt application before they change live behavior. They
do not retroactively rewrite an old lifecycle veto, approve a task blocker,
waive failed verification or merge an unrelated pull request.
