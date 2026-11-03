# ADR-0053: Retire the Filtered view mode; search narrows the chosen scope

Status: accepted (partially supersedes [ADR-0039](0039-desktop-audit-navigation-and-readiness.md)'s Filtered mode mandate)

The canvas has four View Modes: Single, Mesh Grid, Pinned and All Nodes. The
fifth, Filtered, is gone. Search and the provider/status filters are no longer
a mode of their own — they narrow whichever scope the user has chosen, and the
title bar keeps naming that scope while they do.

ADR-0039's other decisions still stand: the readiness steps, the filter
popover's persisted controls, result counts, removable filter chips and single
Clear all action, workspace order stability, the Probe working-set grouping,
Agent History as a Host destination, and the routing catalogue are unchanged by
this record. What this record reverses is one clause: *"Filtered mode keeps its
compact search and adds a popover with all persisted controls, result counts,
removable filter chips and a single Clear all action."* The mode, and with it
the rule that the search field appears only in that mode, are retired; the
popover survives as the Filters control beside the scope label, and the search
survives as a global control in the title bar.

## Context

Issue #2683 asked for a decision on the search model, and the decision was
that search is an operation on a scope rather than a scope of its own.
[ADR-0039] mandated the Filtered mode (#1609) when the canvas header was
content-sized and the search field had to live somewhere: a mode was the only
place a cross-cutting input could be mounted without giving every other mode
its own copy of it.

That answer costs more than it buys:

1. **Two ideas to learn.** A person has to understand both the view scope and
   the special search mode before knowing where agents went. The mode set is
   a scope set plus a search.
2. **A scope that hides the scope.** Entering Filtered left the Mesh Grid, so
   searching from the Mesh Grid changed what the canvas was about — and the
   title bar's scope label changed with it, so the narrowing looked like a
   move.
3. **The rules multiply.** Every mode needs a transition, a fallback and an
   empty state, and Filtered needed a set of its own: its own segment in the
   switcher, its own step in the keyboard cycle, its own persisted mode
   string, and its own empty state beside the grid's own.

## Decision

**1. Four View Modes.** Single, Mesh Grid, Pinned and All Nodes. The Filtered
segment, its funnel glyph and its persisted mode string are gone.

**2. Search narrows the chosen scope.** The scope the user selected stays
selected. Searching inside a Mesh narrows that Mesh; searching in Pinned
narrows the pinned nodes across meshes; searching in All Nodes narrows every
loaded node. The scope label keeps naming the scope, and its count is the
narrowed count — the number of Agent Nodes the current filters leave visible.
The scope's own size is the denominator on the Filters control beside it
("3/12"), so the two numbers are never confused for one another.

**3. One derivation owns the scope.** `deriveScope` in `src/lib/viewModes.ts`
is the single definition of the effective scope — the View Mode, the anchoring
Mesh or none, whether the scope is Mesh-scoped, and the visible and pre-filter
node counts — and the grid render, the title bar, keyboard traversal and the
empty states all read it.

**4. The search is a global control.** The Grid Controls' search input lives in
the title bar's left cluster in every View Mode but Single. It hides there
rather than unmounting, because Single shows one node and has nothing to
narrow. Narrowing is therefore always one gesture away, and the Filters
popover's provider, status and sort controls are reachable from the same
cluster.

**5. Single keeps its return scope and its filters.** A search pressed in
Single returns to the remembered grid first (`lastNonSingleMode`) and then
focuses the search, so a soloed node is not lost to a search that started
nowhere. An active filter never broadens Single's fallback: with filters
active and no matching node, Single renders nothing rather than an unrelated
node.

**6. The Filters popover survives.** All persisted controls (search, provider,
status, sort and direction), the result counts, the removable filter chips and
the single Clear all action are unchanged — they belong to the Filters control
now, not to a mode.

**7. The migration is one-time and explicit.** A persisted `filtered` mode
becomes All Nodes with its active filters retained, so cross-Mesh results
survive the upgrade; filters that were dormant in another mode are cleared
once, because they were never applied to anything; sorting is retained. Mesh
selection is not persisted, so after a reload Mesh Grid asks for a Mesh rather
than widening the scope.

**8. The keyboard cycle loses a step.** Ctrl+Alt+G / Cmd+Alt+G rotates Mesh →
Pinned → All → Mesh. The search shortcut focuses the search within the current
scope instead of entering a mode.

## Alternatives considered

- **Keep Filtered as a mode, but have it remember the scope it came from.**
  Rejected: it keeps the second idea (search as a mode) and the transition
  rules, and papers over only the "a search changed what the canvas was about"
  symptom.
- **Make Filtered a per-scope variant (Filtered Mesh, Filtered Pinned, …).**
  Rejected: it multiplies the mode set by the scope set — six modes instead of
  four, with six sets of transition, fallback and empty-state rules.
- **Clear the filters whenever the scope changes.** Rejected: the user loses
  the query they typed when they click a scope, and changing the scope is
  usually why they are searching again.
- **Move the search into the omnibar only.** Rejected: the omnibar is a
  command palette for jumping to a destination; a persistent, visible,
  clearable query that narrows the canvas is a different thing.
- **Keep the search field inside the canvas header.** Rejected: every surface
  other than the title bar has a content-sized canvas header, so the field
  would sit in a different place in each View Mode — the inconsistency this
  record exists to remove.

## Consequences

- **The title bar's left cluster is wider.** The search moved into it, which
  moved the width ladder that keeps the command field on the window's centre
  line. That ladder is derived from the widest left cluster across both window
  chrome layouts, because the macOS traffic lights live in the same cluster
  (see `src/components/TitleBar/titleBarTiers.ts`).
- **One empty state fewer.** The Filtered empty state is now the
  `filters-exclude-all` branch of the grid's own empty state.
- **A narrowed scope stays visible.** The scope label does not change when the
  user searches, so the canvas and the label can never disagree about what is
  being shown.
- **No backend change.** The View Mode is a UI string union that no backend
  serialises; the migration is a one-time read of persisted UI state.

## Verification

`tests/unit/view-modes.test.ts` pins the four-mode set and the narrowing
predicate; `tests/unit/use-scope.test.tsx` pins the narrowed scope and its
counts; `tests/unit/ui-store.test.ts` pins the one-time migration in both
directions and the retained sorting; `tests/unit/title-bar.test.tsx` and
`tests/unit/view-mode-switcher.test.tsx` pin the retired segment and the
global search control; `tests/unit/grid-shortcuts.test.ts` pins the shortened
keyboard cycle. The browser checks are
`tests/e2e/title-bar-centring-verify-smoke.spec.ts`, which exercises both
window chrome layouts and both interface text sizes. The migration rules are
recorded for developers in
[docs/development/probe-ui.md](../development/probe-ui.md#canvas-layout-view-modes).
