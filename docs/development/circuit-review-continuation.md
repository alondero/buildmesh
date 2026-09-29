# Circuit review recovery journeys

When a review stops, History distinguishes a review limit from an unclear verdict and shows the failure reason and retained report. A graph finishing never counts as reviewer approval.

| Journey | Recovery |
|---|---|
| The reviewer needs one more pass | Review again adds one round to the failed run's limit and queues its next reviewer attempt on the retained implementation worktree. Earlier findings remain in Run History. |
| The approach is wrong | Read the latest findings and open the implementation agent to redirect the work before requesting another review. Continuation is never automatic. |
| The implementation agent was archived after failure | Review again resumes its saved session through the normal spawn lifecycle. A missing session produces an actionable error; it does not silently start over. |
| Cleanup is still stopping an agent | The extension transaction waits for implementation and prior reviewer cleanup before resetting the current projection. |
| Another review already owns the agent | Reuse and reveal that live run. Duplicate clicks cannot create competing reviewers or add rounds twice. |
| Human approval is pending while away | Keep waiting without expiry. Issue collaborator trust and source readiness are separate from the reviewer approving the code. Paused runs must resume before an approval can advance them. |
| The run failed before review, or its custom graph cannot be continued safely | Show the failed step and direct the user to the implementation agent. Do not replay issue implementation or external actions. |
| The worktree/session has been removed | Recover from the PR branch and start a new review. The old ledger still explains what failed. |

Review extension supports the stock node review and issue-driven review blueprint. It keeps the Circuit and Run IDs, increases the run's round limit, and pins a review-only graph with the original reviewer prompt, overrides and feedback instructions. Each replaced graph is retained in the run's snapshot history, so the original immutable run snapshot remains recoverable. This archive is preservation-only for manual/offline recovery; the Probe does not currently expose replaced graphs. PR review scope and instructions to update the PR survive. The next attempt gets fresh observer state and a fresh reviewer; implementation and publication steps are not replayed. Previous step transitions and findings remain in Run History. Existing automatically created Continued Review circuits are consolidated under their original Circuit on upgrade, with each historic run and its pinned snapshot preserved.

An explicit extension admits even if the issue review Circuit was disabled after its original run; disabling still parks new background triggers. An implementation agent originally owned by that Circuit remains counted against the optional global agent pool while the extended review borrows it.

The review handoff accepts a clean finished turn even when the overall task is unfinished. Ambiguous permission/input waits still require inspection; clicking Start Review does not answer a question in the implementation terminal.
