# 49. Retire the sidebar Attention view and the Active band label

Status: accepted (partially supersedes [ADR-0039](0039-desktop-audit-navigation-and-readiness.md)'s sidebar Attention view mandate)

The desktop sidebar has exactly one view. The Workspace/Attention toggle above the
mesh list is gone, the "Active" heading inside the list is gone, and the grouped
attention view they selected is gone with them. Attention is reported where the
work lives — on the mesh row, on the node's status mark, on the node's recovery
controls, and in Agent History.

ADR-0039's other decisions still stand: the readiness steps, filtered mode and
its persisted controls, the Probe working-set grouping, Agent History as a Host
destination, and the routing catalogue are unchanged by this record. What this
record reverses is one clause: *"A separate sidebar Attention view lists failed,
waiting and suspended nodes across repositories, with direct terminal/recovery
actions; it never changes the workspace's drag order."*

## Context

The sidebar carried a row of chrome above the mesh list, and the mesh list itself
carried a second.

1. A **Workspace/Attention segmented toggle** (issue #2467) switched the column
   between the mesh list and a grouped attention view.
2. An **"Active" band label** (issue #1939) headed the group of meshes that have
   at least one open node.

Both were additive chrome around a list that already carried the same
information by position. The project's own sidebar audit
([docs/prototypes/mesh-sidebar-variants.html](../prototypes/mesh-sidebar-variants.html))
lists "dual Workspace/Attention tabs plus an Active label" among the noise
candidates to cut, alongside permanent drag handles and provider icons on every
row.

The attention view itself was not earning its place. Its three groups —
failed sessions, waiting sessions, paused sessions — restated a state each
agent already reports in place: a mesh holding an agent that needs input, has
errored, was lost, or is suspended opens itself, that agent's status mark names
the state, and the node's own row carries the recovery control (**Restart** for
a failed session, **Resume** for a paused one that has a saved session
identity). The view's cross-repository
reach was already covered by **Agent History**, whose Lifecycle view lists
agents from every repository, filters them by lifecycle state, and offers
**Open terminal** on each row, plus **Retry** for a failed or lost one,
**Resume** for a paused one and **Reopen** for an archived one.

## Decision

**1. The sidebar has one view.** The Workspace/Attention toggle is removed, and
with it the `AttentionList` component, its grouped attention cards, the
attention count it showed, and the attention-only route into the column.

**2. The mesh list carries no band label.** Meshes with at least one open node
still sort above the rest and the remaining rows still render dimmed; only the
heading that named the group is removed. The position in the list is the
affordance.

**3. Attention stays visible where the work lives.** An agent's status is no
harder to see: a mesh whose member needs input, has errored, was lost, or is
suspended auto-expands, the node's status mark names the state, the node's row
keeps its restart and resume controls, and Agent History lists agents across
repositories with a lifecycle-state filter and the same recovery actions.

**4. The mobile attention deck is untouched.** The phone client has its own
attention surface — a "Waiting for input" deck on Overview — which this record
does not change.

## Alternatives considered

- **Keep the component, unlink the toggle.** Rejected: the view becomes
  unreachable code that still needs a test file and a maintenance owner, and it
  rots silently — nothing renders it, so nothing tells you it broke.
- **Move the attention list into the Probe panel as another tab.** Rejected for
  now: it trades a sidebar row for a Probe destination, which is new surface
  area for a view nobody used. The same three jobs — listing agents across
  repositories, narrowing them by state, and recovering them — are already done
  by Agent History's Lifecycle view.
- **Keep the "Active" label and drop only the toggle.** Rejected: the label is
  the half of the pair that carries the least. It names a distinction the
  ordering already shows, and it costs a row of a list the user reads at a
  glance.
- **Keep both, and shrink them.** Rejected: there is no smaller form of a
  label that names what the order below it already shows.

## Consequences

- **The column starts with the work.** The sidebar's mesh list moves to the top
  of the column; one chrome row and one label row are gone from the default
  256px-wide strip that the user reads at a glance.
- **Every action survives.** The attention view's actions exist on the node's
  own row or in Agent History, in the place the agent already lives.
- **The attention view is no longer a navigation destination.** ADR-0039's
  readiness, filtering, Probe and Agent History decisions are unaffected; only
  its sidebar Attention view mandate is reversed by this record.

## Verification

`tests/unit/sidebar-single-view.test.tsx` pins the removal of the toggle and the
retired attention groups; `tests/unit/sidebar-mesh-bands.test.tsx` pins the
absent band label and the surviving ordering and dimming;
`tests/unit/sidebar-empty-workspace-cta.test.tsx` pins the one mesh-creation CTA
per workspace state; the Attention triage cases in
`tests/unit/desktop-audit-followup.test.tsx` are removed with the feature.

The retired view's test file and the triage cases in
`desktop-audit-followup.test.tsx` were the only coverage of "a failed agent can
be retried from a cross-repository list, a duplicate retry is refused, and a
refused recovery is reported". `tests/unit/agent-history-recovery.test.tsx`
pins that contract on the surface that took the view over, so retiring the
view did not retire the behaviour.

One gap that retirement exposed is filed separately: Agent History offers
**Resume** on a paused agent that was never given a session identity, where
the backend refuses the resume ([issue #2650](https://github.com/alondero/buildmesh/issues/2650)).
The retired view gated that control on a saved identity; Agent History does
not. It is left as filed, so this record does not change the Probe's recovery
behaviour.

See the [user guide](../user-guide.md#find-agents-that-need-attention) for the
surviving attention surfaces.
