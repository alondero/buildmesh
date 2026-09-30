# Circuit indicators on Agent Nodes

Status: Implemented.

## Goal

Make Circuit ownership visible wherever Agent Nodes are scanned, while keeping
Circuit state separate from the Agent Node lifecycle. Nodes without Circuit
ownership render no indicator.

## Presentation contract

| Presentation | Meaning | Suggested accessible label |
|---|---|---|
| Active | A running Circuit is driving a running or spawning node. | `Circuit active` |
| Waiting | Circuit ownership is live, but the node is waiting, paused, queued, or needs recovery. | `Circuit waiting` |
| Done | The Circuit run completed; the Agent Node remains available for inspection. | `Circuit done` |
| Needs attention | The Circuit failed or its state is unknown. | `Circuit needs attention` |
| None | No live or retained Circuit ownership applies, or the Circuit was cancelled. | Render nothing |

The lifecycle dot remains authoritative for the Agent Node itself. `ready`
means the agent yielded cleanly; it does not mean a person is needed. An error
or lost node under live Circuit ownership needs attention. A failure after a
Circuit has completed belongs to the agent and should use agent-facing copy.
Archived nodes have no indicator.

The shared presentation seam is `src/lib/circuitNodePresentation.ts`. It maps
the Agent Node lifecycle and `CircuitAgentOwnership` into phase, tone, label,
and detail. Unknown ownership states receive an error treatment instead of
being presented as active or complete. Card-level attention aggregates every
member that needs input or has failed, and keeps the primary member's details
and reveal action in sync.

## Layout and interaction

Use the Pilot-light glyph in a fixed 14px ownership cell in the canvas header
and sidebar. Keep the cell empty for unowned nodes so adjacent lifecycle,
provider, and name columns remain aligned. The indicator includes an accessible
name and tooltip; its optional action opens the owning Circuit run in the
Circuits Probe.

The card-level attention chip remains separate from the Pilot light. A solo
focused failure is already visible in the light and does not get a duplicate
chip. Multi-member cards retain the chip so users can reach every failing or
waiting member.

## Refresh behavior

`circuit-run-updated` patches known ownership rows and refreshes the ledger for
live runs. Terminal events refresh Agent Nodes after Circuit cleanup. The
`circuit-pr-ready` event invalidates the owning node's open-PR cache when an
OpenPr action succeeds, so the PR pill updates without waiting for a later git
event or remount.

## Acceptance evidence

- Rust tests cover Circuit ownership retention, terminal history, and cleanup.
- Unit tests cover lifecycle-to-presentation mapping, failure attribution,
  cancellation, and unknown ownership.
- Component tests cover accessible labels, tooltips, fixed ownership cells,
  and shared header/sidebar behavior.
- Listener and cache tests cover Circuit state refresh and PR cache invalidation.
