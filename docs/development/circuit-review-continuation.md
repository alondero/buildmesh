# Circuit review recovery journeys

When a review stops, History distinguishes a review limit from an unclear verdict and shows the failure reason and retained report. A graph finishing never counts as reviewer approval.

| Journey | Recovery |
|---|---|
| The reviewer needs one more pass | Continue review starts a one-round follow-up on the retained implementation worktree. The failed ledger remains unchanged. |
| The approach is wrong | Read the latest findings and open the implementation agent to redirect the work before requesting another review. Continuation is never automatic. |
| The implementation agent was archived after failure | Continue resumes its saved session through the normal spawn lifecycle. A missing session produces an actionable error; it does not silently start over. |
| Cleanup is still stopping the agent | The creation transaction rejects continuation while cleanup owns the node. Once a follow-up borrows it, subsequent cleanup cannot claim it. |
| Another review already owns the agent | Reuse and reveal that live run. Duplicate clicks cannot create competing reviewers. |
| Human approval is pending while away | Keep waiting without expiry. Issue collaborator trust and source readiness are separate from the reviewer approving the code. Paused runs must resume before an approval can advance them. |
| The run failed before review, or its custom graph cannot be continued safely | Show the failed step and direct the user to the implementation agent. Do not replay issue implementation or external actions. |
| The worktree/session has been removed | Recover from the PR branch and start a new review. The old ledger still explains what failed. |

Continuation supports the stock node review and issue-driven review blueprint. It creates a disabled, reusable manual circuit with the original reviewer prompt and overrides, plus the original feedback instructions targeted at the borrowed implementation agent. PR review scope and instructions to update the PR survive. The new run records its predecessor in context; it gets fresh observer state, a fresh reviewer, and its own bounded review allowance. The original implementation and publication steps are not replayed.

The review handoff accepts a clean finished turn even when the overall task is unfinished. Ambiguous permission/input waits still require inspection; clicking Start Review does not answer a question in the implementation terminal.
