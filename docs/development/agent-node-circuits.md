# Circuits from Agent Nodes

Use the Circuit icon beside Build/Run in an Agent Node's title bar. Choose
**Automated review loop** or a saved Circuit with a manual trigger on the same
Mesh. Saved Circuits keep their configured timing and actions; the review
preset waits for the source task to finish.

The review preset classifies the source's completion report, starts a separate
reviewer, returns findings to the original agent, waits for fixes, and reviews
again. Explicit reviewer approval ends the loop. The default limit is three
review rounds, configurable from one to ten. Reaching the limit or receiving
an unclear reviewer verdict ends the run in a failed, recoverable checkpoint;
it does not claim approval. Resume the saved implementation session to resolve
the blocker or request a fresh review. When the source's completion cannot be
read or classified, a visible approval step lets the user confirm completion
before review begins.

The reviewer uses the provider picked in the Start Review dialog when one is
chosen, otherwise the configured Reviewer provider, otherwise the source
agent's provider. Model and effort come from a picked Launch Configuration
when one is chosen, otherwise the selected reviewer's harness-specific Mesh
overrides, then application defaults. Legacy source snapshots and shared
preset graphs do not override that configuration.
It receives its working directory, Mesh base ref, name, and latest completion report. It is instructed
to review committed changes from the merge-base and uncommitted/untracked
changes, without editing files or posting to GitHub. It has its own worktree;
no commit, push, or PR is required for this workflow.

## Completion and recovery

Circuits can progress from a finished, readable agent report even when the harness
cannot prove a complete inventory of background work. The report must belong to
the current session and input, and remain unchanged through the decision commit.
Known unfinished work, actual permission requests, and actual questions still block
progress. A status-only waiting-for-input signal does not create an indefinite
human request. Report-based progress is recorded separately from verified native
lifecycle evidence; it is not proof that an unobservable background task ended.

Unverified agent steps continue checking for fresh evidence without spawning again
or resending prompts. Removing an agent, or a confirmed agent error, terminates the
waiting run so queued runs can use its capacity. Ambiguous GitHub actions and prompt
deliveries retain their existing manual reconciliation controls.

Issue-driven review runs request an explicit final result from the implementation,
wrap-up and feedback phases: `BUILDMESH_HANDOFF_V1: READY` or `BLOCKED`. A valid
result advances its matching report gate without calling the Circuit classifier,
after the same session, input, freshness and known-work checks. READY means the
assigned phase is ready for its next gate; independent review still decides
approval. Older reports without a result use the configured classifier, and an
exhausted classifier budget still requires an explicit evidence recheck.

When an agent disappears, its cancellation explanation retains the preceding
checkpoint. Siblings stopped by a failed run identify that failure as their cause.
See the [agent loss audit](circuit-agent-loss-2026-10-02.md) for the historical
classifier and identity failures hidden by the old closure message.

## Review presentation contract

The title-bar review preset and the issue-driven Autopilot review blueprint
use the same activity presentation and review criteria.
The source is the **Implementation** activity and each owned reviewer is a
**Review** activity in the source's node card, selected through the shared
activity tabs. Selecting a review activity retargets the header actions,
terminal, input, and changes to that reviewer; the source task title remains
stable. The All sessions menu remains available when the card is narrow.

Review findings are returned through the circuit run's existing feedback step
to the source agent. The source's terminal remains the place where that agent
receives the requested fixes, while the reviewer report remains available in
the run history and on the reviewer activity. Both flows use explicit approval,
changes-requested, and blocked verdicts. Only changes-requested reports start
a fix round. A blocked or exhausted run preserves the reviewer checkpoint,
including its association and worktree, for recovery; cleanup stops its live
process and is retryable rather than deleting the review evidence.

By default, reviewers inherit the reviewed agent's harness. The app-wide
**Reviewer provider** setting in Settings can override that fallback for
adversarial review; it is snapshotted into each run, so changing Settings does
not alter a queued review. The Start Review dialog's **Reviewer provider**
picker overrides the app-wide setting for that one run only; it renders the
same harness-grouped Spawn Menu as every other spawn surface, minus Terminal,
with each harness's Launch Configurations in its disclosure. The built-in
review preset is shared per Mesh, so
it never stores a provider-specific default from the first agent that used it.
Explicit reviewer settings in an authored Circuit take precedence; configure
those in the reviewer `SpawnAgentNode` inspector's Provider field. Model and
effort use the existing Mesh/harness cascade. PR reviews inspect the published PR and post
their findings there as well as returning the terminal report. Local reviews
include uncommitted work and do not publish comments.

Automated first-turn prompt delivery has one shared policy in the agent launch
module. It uses a harness startup prefill only when the adapter says that the
prompt is safe for the command line; otherwise it starts the process cleanly
and pastes the prompt through the PTY. Codex uses the latter for multiline
review/diff text, avoiding CLI argument parsing of diff lines such as `+ ...`.
For a multiline Codex prompt, including one using a proxied Codex provider,
Buildmesh waits until Codex renders the pasted content in its input box and the
redraw settles before sending Enter. The wait captures an output position before
the PTY write, accepts Codex's normalized line-ending count, and uses the completed
paste marker for long prompts. A startup redraw alone cannot acknowledge the
paste; if Codex never renders it, the node is marked for attention instead of
leaving an apparently submitted review idle. A dead process ends the wait early.
Output that arrives immediately after Enter can acknowledge that keystroke.
Ordinary interactive spawn intents retain their user-facing startup-prefill
behavior because they have different submit/readiness semantics, but both
paths share prompt construction and harness argument preparation.

On startup, inactive stock review graphs are upgraded to this contract while
preserving spawn settings and round limits. Customized issue-review prompts
or topology are left intact. Active runs retain their saved graph; their
upgrade is reconsidered on a later startup after they finish.

Runs use the existing Circuits queue, capacity limits, run history, pause,
approval, and cancellation controls. A repeated start while the source already
has a pending/active node-started run returns that run. An agent actively owned
by another Circuit or legacy Autopilot must finish that automation first, so
two controllers cannot send it competing instructions. Suspended agents must
be resumed before starting a workflow.

## When a review can start

A review or authored Circuit must pass two source checks: the IPC command
requires a live process, and the ledger mint path requires status
`Running | AwaitingInput | Completed | Ready` — resume a suspended agent first.
There is no separate "the source has been observed" precondition: a source
whose worker has not yet captured a session identity or a readable report
still starts.

The run's first step waits for the source to yield a turn, so a source that has
not finished its first turn is fine — the review simply waits for it. That wait
is bounded: if the source never becomes observable, the [#1791
watchdog](../releases/v1.4.0.md#circuit-watchdog-fails-fast-on-never-observed-agents-1791)
ends it after 15 minutes with "agent produced no session identity or report
within 15 minutes — inspect the agent terminal" instead of holding the run for
the full active-wait budget, as in the [Muse Index review](circuit-run-183-muse-index.md).

The title-bar control is stricter than the backend on one axis: it disables
itself when the *source* harness cannot yield a turn (for example, a plain
Terminal, or a harness with neither a native attention hook nor a passive turn
watcher), because `await_source` and `verdict` would otherwise park forever
waiting for a status the harness never reports. The reviewer must likewise be an
eligible harness. The first-writer-wins dedupe (issue #1660) still hands back the
original run id on a retry.

## Unverified checkpoints and evidence history

An **Unverified Checkpoint** holds the current attempt when its evidence window
expires or an external action may have been sent without a recorded result.
Open **Circuit Run History** in the expanded run to inspect recorded transitions,
observations, action intent, possible dispatch, and operator decisions.

For an uncertain GitHub action, inspect GitHub before recording **completed** or
**not performed**, and supply a reason and supporting evidence. These records
are operator attestations. They do not grant tool permission or review approval.
Recording **not performed** leaves the checkpoint unresolved and offers a
separate **Retry as new attempt** action. A stale action requires refreshing the
history. Unknown GitHub effects are never automatically replayed.

The complete action-by-action journal, provider contract, cancellation, and
restart inventory is in the [Circuit effect recovery contract](circuit-effect-recovery.md).

Agent checkpoints offer **Recheck evidence** for the same attempt. This restarts
the observation window without sending the original prompt or requesting an
automatic continuation. Eligible agent handoffs also offer **Record completed**:
inspect the work, enter a reason, and attest that this step can advance. This is
an operator decision, not verified lifecycle evidence. Missing agent attachments,
known unfinished child work, conflicting evidence, and unresolved human requests
cannot be completed this way; review-verdict and approval gates remain separate.
Recovery controls appear before the retained history entries.

If a feedback prompt is staged in a terminal, submit it there and confirm that
the harness accepted it before recording the prompt-delivery step as completed.
Do not mark it not performed and retry while the staged text remains: that can
duplicate the prompt. A changes-requested review can authorize feedback recovery;
it does not need an approved verdict first.

Uncertain OpenPr actions with a saved repository and branch get up to five
automatic read-only lookups, a minute apart, on the same attempt. A matching PR
restores its identity and advances the run without creating another PR. The
allowance survives restart; after it is exhausted, **Recheck evidence** remains
available for a deliberate lookup.

Observed human-input waits and explicit approval gates
are excluded from the evidence deadline; native wait coverage across harnesses
is still being integrated.
A yielded foreground turn alone cannot complete a spawn that was assigned work.
Native child/background observation is still being integrated; the current
status projection has reduced confidence and cannot prove complete ownership.
See the [acceptance record](circuit-reliability-acceptance.md) for the remaining
implementation and live-verification gaps.

Review Runs pin their graph, reviewer launch configuration and behavior revision
when created. Later model, effort, argument, runtime or provider-route edits
apply to future runs. Continue carries the retained configuration into its
successor. A copied review remains continuable while its borrowed source,
reviewer, explicit verdict, feedback, approval and bounded-loop structure remain
verifiable; changing that structure requires manual recovery.

## Blueprint authors

On prompt injection and classifier steps, select **Triggering agent** to target
the source (`target_node_id: "$source"`). It is a borrowed reference in the run
context, not an owned spawn step. Close/status actions cannot target it; normal
cancellation and reviewer cleanup preserve the original Agent Node and worktree.

Node-started runs supply `source.agent_id`, `source.name`, `source.path` (the
canonical spawn path), and `source.base_ref` (the Mesh's review baseline).
`source.output` becomes available after a source completion classifier runs.
These variables appear in the editor's template reference. Ordinary Trigger
Now has no source context and rejects graphs requiring a source binding.

`AwaitAgentTurn` classifies completion using a readable transcript even when
the source finished before the run started. `ReviewVerdict` routes `completed`
for explicit approval, `working` for findings requiring changes, and `blocked`
for an unclear/incomplete review or a classifier failure. These are routing
outcomes, not agent lifecycle statuses. Circuit reviewer prompts now ask for a
final `BUILDMESH_REVIEW_V1: APPROVE`, `REQUEST_CHANGES`, or `BLOCKED` line
(the prefix is required for each value). Fix feedback in the node-review preset
asks for `BUILDMESH_HANDOFF_V1: READY` or `BLOCKED`. Valid result lines route
without another model call; malformed or conflicting result lines cannot approve.
The full report still carries findings and verification. Legacy free-form reports
reuse the configured Autopilot classifier and existing verdict fallback.

A fresh explicit result can establish report readiness despite a stale Running
status. Session, input, report-revision, known child-work and human-request checks
still apply. This is report-based handoff, not verified lifecycle completion.
Dispatch appends the contract to saved/custom prompts without changing their
review scope. Existing historical reports are not rewritten or auto-approved.

The implementation lives in `circuit/node_review.rs`, the pure
stepper, and the existing Circuit worker. The built-in review graph is a
per-Mesh preset row (`is_preset = 1`) reused by every invocation and excluded
from user-authored blueprint lists; old duplicate preset rows are collapsed by
the schema migration. Each run stores its borrowed source in the indexed
`source_agent_node_id` foreign key (with the context copy retained for
templates), while only reviewers occupy owned `agent_node_id` step
associations. The reviewer footprint reserves one additional automated-agent
slot. The Circuits Probe offers **Inspect Review Blueprint**. The inspector is
read-only and offers **Copy to editable Circuit**, which creates an independent,
disabled manual Circuit on the same Mesh. Copying transfers no runs or history.
The preset retains its reports and has no trigger, enable, or delete controls.
Worker source-health checks, ownership, and cleanup use the relational source
binding; graph template expansion and target resolution retain the context
copy.


## Legacy automation retirement

Startup cancels active legacy Autopilot runs before automatic session recovery,
disables legacy scheduling, and durably queues owned processes to stop. A stop
that is interrupted is retried without dispatching more work. Agent Nodes,
worktrees, PR identities and legacy history are retained. Cancelled legacy work
is excluded from automatic resume. Legacy enable commands reject activation.

The Autopilot Probe destination now exposes retained settings read-only and a
link to Circuits. Configure Circuits manually: no legacy settings or runs are
converted. Circuit run capacity remains independently editable and does not
inherit the old legacy concurrency value.


### Human request evidence

Circuit history distinguishes input, permission, ordinary questions and review
approval. Identified Claude/Codex callbacks retain their request ID and originating
session/turn; only a matching response resolves that request. A tool result may
resolve its matching tool question and permission, but supplies no completion
proof. Generic Working, Stop and prompt-submission signals do not answer requests.
Missing request correlation remains visible with reduced confidence and no automatic
timeout. Status projections retain their lifecycle-transition time so a delayed
snapshot cannot recreate a request already answered by newer native evidence.
Paused and terminal runs show retained waits as history, without active-wait actions.

Codex permission callbacks that omit a request ID retain an uncorrelated permission
wait. An unrelated tool result cannot resolve that wait. Inspect the harness's
permission state and the recorded evidence; generic activity is not approval.


### Evidence reconciliation history

The built-in Review Blueprint is available for inspection and copying before the
first review run. Its graph, settings and history cannot be deleted or edited as
an individual Circuit; deleting its entire Mesh remains a separate operation.

Run History records pinned behavior/graph identity and effective reviewer selection,
model and effort, evidence-window changes, run and step capacity waits, added
review rounds, and continuation prompt intent, possible dispatch and outcome.
Unchanged polling results do not add repeated entries. Arguments, endpoints and
prompts are excluded from the configuration history summary. Historic linked
review runs retain their lineage entry after consolidation under the original
Circuit. A retention sweep removes a run together with its history. Every entry
additionally names its source (who/what
produced the event) and disposition (what Buildmesh did with it) beside its
identity and time, so waits, capacity waits, configuration pins and recovery are
diagnosable uniformly; a cleared wait records `resolved` and keeps the attempt it
was parked on.

Conflicts retain their cause and triggering evidence identity. A validated Codex
foreground pull may resolve only the matching foreground conflict, and its file
fingerprint is checked again before persistence. Ownership, identity and legacy
conflicts remain unresolved. Foreground reconciliation cannot supply missing
child/background coverage, answer a human request, or approve a review.
