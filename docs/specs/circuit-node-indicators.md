# Circuit indicators on Agent Nodes

Status: Implemented.

## Goal

Make Circuit ownership visible wherever Agent Nodes are scanned, while keeping
Circuit state separate from the Agent Node lifecycle. Nodes without Circuit
ownership render no indicator.

## Presentation contract

| Presentation | Meaning | Orbit ring | Accessible label |
|---|---|---|---|
| Active | A running Circuit is driving a running or spawning node. | Violet comet circling the status circle | `Circuit active` |
| Waiting | Circuit ownership is live, but the node is waiting, paused, queued, or needs recovery. | Amber half ring, held still | `Circuit waiting` |
| Done | The Circuit run completed; the Agent Node remains available for inspection. | Green closed ring | `Circuit done` |
| Needs attention | The Circuit failed or its state is unknown. | Red dashed ring | `Circuit needs attention` |
| None | No live or retained Circuit ownership applies, or the Circuit was cancelled. | No ring | None |

The status circle remains authoritative for the Agent Node itself. `ready`
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

A node is one 20px glyph in the canvas header and the sidebar: the status
circle with, while Circuit owns the node, the orbit ring around it
(`NodeStatusGlyph`). There is no separate ownership cell, so an unowned node
is just the circle. The circle is the node's lifecycle status; its fill style
(solid, hollow, dashed, slashed, half-filled, ringed dot, thin, cross) is part
of the status vocabulary in `src/lib/status.ts`.

The shape+colour collision set is exactly two pairs, both deliberate:
`pending` and `spawning` (the same "starting" state mirrored from two
code paths) and `completed` and `ready` (pre-existing ✓/green grouping).
`idle` and `running` are not a collision: they share cyan but differ in
shape (hollow ring vs solid circle). A test in
`tests/unit/node-status-glyph.test.tsx` guards that a future change does
not introduce a third such pair.

The orbit shapes are chosen so every Circuit state differs in outline, not
just colour. The comet silhouette (tapering tail and star head) belongs to
Active alone. The comet turns once every 3.2 seconds and every comet shares one
page-wide clock, so a grid of piloted nodes moves in step. With
`prefers-reduced-motion` the comet parks at 12 o'clock over a stronger track,
which still differs from every parked ring.

The glyph has one accessible name that joins the node status and the Circuit
state (`Running. Circuit active`) and a tooltip that adds the Circuit detail.
Its optional action opens the owning Circuit run in the Circuits Probe; when it
has one, the glyph is a button with a 24px target that does not change the
row's layout.

The card-level attention chip remains separate from the glyph and keeps its own
Pilot-light icon. A solo focused failure is already visible as the dashed red
ring and does not get a duplicate chip. Multi-member cards retain the chip so users can reach every failing or
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
- Component tests cover the orbit shape for each presentation, the combined
  accessible name and tooltip, the single fixed-size glyph box, the click seal
  on the action button, the shared comet clock, the reduced-motion stylesheet
  rule, and shared header/sidebar behavior.
- Listener and cache tests cover Circuit state refresh and PR cache invalidation.
