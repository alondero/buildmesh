# 31. Palette tool discovery start screen

## Status

Accepted for the command palette's first-open state (2026-09-03). Builds on
[ADR-0030](0030-titlebar-navigation-on-demand-inspector.md): the palette
remains the primary navigation surface, and the lens contract from
[ADR-0029](0029-probe-context-lenses.md) still governs data ownership — it
just stays out of the navigation language.

## Context

Removing the Probe rail left first open showing "No matching results" until
the user typed, so destinations were undiscoverable. Earlier feedback also
called out that the scope of each tool and the grouping were hard to figure
out. Four open-state prototypes were compared (`prototype/palette-discovery`,
variants A–D); task-grouped tools above the search bar (A) was picked, with
the scope pills of (B) folded in as per-group scope notes.

## Decision

- **An empty query shows grouped destination shortcuts in the results
  area below the search input** instead of the empty state. The input
  stays anchored on top at all times: typing swaps the grid for filtered
  results in place, with zero layout shift. A genuinely non-matching
  query still shows "No matching results".
- **Groups are Code, GitHub, Automate, Remember, App-wide.** GitHub Issues
  and Pull Requests share one group. Every group carries a scope note
  ("Selected project …", "App-wide — ignores project selection"); the Code
  group additionally names its exception (Agent Changes follows the focused
  agent).
- **One name per destination**, following the `Open <Header>` palette
  pattern: tiles show the inspector header title, search rows the `Open …`
  command, both resolved from the same `PROBE_DESTINATION_COMMANDS` +
  `PROBE_TAB_DEFINITIONS` sources.
- **Grouping lives in a presentation-layer module**
  (`src/components/CommandOmnibar/toolDiscovery.ts`), not the search
  indexers: static group literals plus one type-level exhaustiveness
  assertion, with tiles pre-resolved once at load so the render pass
  allocates nothing. Adding a destination fails the build until it is
  placed in a group.
- **Tiles open their destination directly** (`openProbeTab(tab)` + close):
  the tab id is already known, so there is no round-trip through the
  search index and command-id dispatchers.
- **Keyboard parity without focus theft.** Tiles are real buttons
  (Tab-reachable, natively activatable, `focus-visible` ring), while
  ArrowUp/Down move a virtual highlight across the grid with DOM focus
  anchored in the input — typing is never interrupted, Tab order is
  untouched, and Enter activates the highlighted tile. Groups are
  `role="group"` regions with accessible names; the input tracks the grid
  as its popup (`aria-expanded`/`aria-controls`/`aria-activedescendant`).

## Consequences

- Adding a destination now means three placements that all fail loudly:
  `PROBE_TAB_ORDER`, `PROBE_DESTINATION_COMMANDS`, and a tab tuple in
  `src/components/CommandOmnibar/toolDiscovery.ts` (the
  `_AssertDiscoveryExhaustive` check fails the build until it is placed).
- Pre-existing header/row label drift (`Worktree Manager` vs `Open
  Worktrees`, `Pull Requests` vs `Open GitHub Pull Requests`) is unchanged
  by this decision and tracked separately.
- Group headings are navigation aids, not task destinations: they are not
  part of the one-name rule and must not leak lens jargon (Host/Mesh/Agent)
  into user copy.
