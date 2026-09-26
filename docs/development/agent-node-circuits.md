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

## Source-agent readiness gate (#1792)

The built-in review preset refuses to mint a run on a source agent that has
not produced any observable evidence yet — neither a captured `cli_session_id`
nor a readable `assistant_report` revision. A freshly-spawned node reaches
`status = Running` before the worker has had time to read a session identity
from the harness's transcript dir or capture a readable report, so without
this gate the review would burn the full `ACTIVE_WAIT_MS` budget waiting for
evidence that never arrives. When the gate fires, the dialog shows:

> Source agent has not started yet — wait for its first turn before starting
> a review.

The dialog opens a **Review an agent that hasn't started yet** checkbox in
the same panel as the rounds control. Ticking it bypasses the gate and the
override is recorded on the run's `context_json` (`source.review_allow_unobserved
= "1"`) so the audit trail shows which runs skipped the readiness check. The
checkbox defaults to off, so the default behaviour is to wait for the source
to be observed.

The gate is **only** enforced on the built-in review preset:

- **Recovery** (Continue review after a failed run) bypasses the gate: the
  source is already known to be observed via its previous run's evidence.
- **Explicit user-selected Circuits** (any authored blueprint chosen from the
  picker) bypass the gate: the user is asking for a specific blueprint, not
  the built-in review preset.
- **Recovery-via-IPC entry points** (Continue review, the reviewer's feedback
  step) call the recovery path internally and therefore inherit its
  permissive behaviour.

The override does not soften any other gate: the reviewer still has to be
alive, the source still has to be in `Running | AwaitingInput | Completed |
Ready`, and the first-writer-wins dedupe (issue #1660) still hands back the
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

Agent checkpoints offer **Recheck evidence** for the same attempt. This restarts
the observation window without sending the original prompt or requesting an
automatic continuation. Observed human-input waits and explicit approval gates
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
outcomes, not agent lifecycle statuses. Both gates reuse the existing Autopilot
classification backend configuration.

The implementation lives in `autopilot/circuit/node_review.rs`, the pure
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
model and effort, evidence-window changes, run and step capacity waits, and
continuation prompt intent, possible dispatch and outcome. Unchanged polling
results do not add repeated entries. Arguments, endpoints and prompts are excluded
from the configuration history summary.

Conflicts retain their cause and triggering evidence identity. A validated Codex
foreground pull may resolve only the matching foreground conflict, and its file
fingerprint is checked again before persistence. Ownership, identity and legacy
conflicts remain unresolved. Foreground reconciliation cannot supply missing
child/background coverage, answer a human request, or approve a review.
