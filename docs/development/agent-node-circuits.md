# Circuits from Agent Nodes

Use the Circuit icon beside Build/Run in an Agent Node's title bar. Choose
**Automated review loop** or a saved Circuit with a manual trigger on the same
Mesh. Saved Circuits keep their configured timing and actions; the review
preset waits for the source task to finish.

The review preset classifies the source's completion report, starts a separate
reviewer, returns findings to the original agent, waits for fixes, and reviews
again. Explicit reviewer approval ends the loop. The default limit is three
review rounds, configurable from one to ten. Reaching the limit reports that
the latest fixes remain unapproved. An unclear reviewer verdict stops for
attention. When the source's completion cannot be read or classified, a visible
approval step lets the user confirm completion before review begins.

The reviewer uses the source agent's provider plus the Mesh's configured model
and effort tier, and receives its working directory, Mesh base ref, name, and
latest completion report. It is instructed
to review committed changes from the merge-base and uncommitted/untracked
changes, without editing files or posting to GitHub. It has its own worktree;
no commit, push, or PR is required for this workflow.

Runs use the existing Circuits queue, capacity limits, run history, pause,
approval, and cancellation controls. A repeated start while the source already
has a pending/active node-started run returns that run. An agent actively owned
by another Circuit or legacy Autopilot must finish that automation first, so
two controllers cannot send it competing instructions. Suspended agents must
be resumed before starting a workflow.

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
slot. Startup reconciliation and lost-turn recovery use the relational source
binding and the same run context for template expansion.
