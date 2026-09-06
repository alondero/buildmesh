# 32. Probe working-set tabs inside the on-demand inspector

## Status

Accepted for the Probe navigation model (2026-09-06). Extends
[ADR-0030](0030-titlebar-navigation-on-demand-inspector.md) (title-bar-first
navigation, on-demand inspector); does not supersede it. Builds on the tool
discovery vocabulary of [ADR-0031](0031-palette-tool-discovery.md).

## Context

ADR-0030 left the open inspector with no in-panel switcher: destinations are
reached through the palette, the title bar, and contextual entries. Post-launch
use showed the cost of that model concentrated in one place — alternating
between two or three destinations (the common loop: check Files → review Agent
Changes → back) requires a round trip through the palette every time, and the
inspector gives no visibility into which destinations exist beyond what the
user remembers to search for.

A four-variant prototype (icon rail, pill scroller, working-set tabs, hub
dropdown) was built and reviewed against the three ADR-0030 objections to
the old activity rail. The working-set tab strip won:

- **Width** — the old rail took permanent width from the workspace *while
  closed*. A rail rendered only inside the open inspector takes none; closed
  still means GONE (the closed-render discipline of `ProbePanel` is
  untouched).
- **Icon-only confusion** — the rail's tabs carry labels (collapsing to
  icons only at narrow panel widths, where `probe-ui-checklist.md` §2
  applies), and the full destination list opens as a menu of the palette's
  *labelled* tool-discovery tiles.
- **Internal lens groupings as tasks** — the menu reuses ADR-0031's
  `TOOL_DISCOVERY_GROUPS` (Code / GitHub / Automate / Remember / App-wide),
  which are user tasks, not lens vocabulary.

The prototype's "+" affordance for opening that menu was rejected in review:
"+" reads as *create a new tool*, not *browse the rest of them*. The shipped
glyph is Lucide `layout-grid` — app-launcher language for "browse all tools",
and a deliberate echo of the tool grid it opens.

## Decision

- **A working-set tab strip renders between the inspector header and the tab
  body** (`ProbeToolRail`). It lists only destinations opened this session,
  capped at four (`PROBE_WORKING_SET_CAP`); the least recently visited entry
  is evicted beyond the cap. Chrome scales with actual usage, not with the
  full destination list.
- **Display order and recency are separate concerns.** The strip renders
  destinations in *insertion order* — a new tab appends at the end,
  survivors never move, and activation does not reorder anything. Recency
  (`mru`) drives eviction only. Arrow-key navigation walks the stable
  display positions, so every destination in the working set is reachable
  with ArrowRight and ArrowLeft; re-rendering the strip in recency order
  (an early draft) trapped every tab past index 1 behind an ArrowRight
  ping-pong and made ArrowLeft dead code, which review caught before merge.
- **Every activation records a working-set visit.** `setProbeTab` pushes to
  `probeWorkingSet`, so `openProbeTab` (palette, title bar, contextual
  entries) and rail clicks feed the same set, and `toggleProbe` records a
  visit when it opens the panel — whichever entry point opens the dock, the
  rail never renders empty and the body's `aria-labelledby` always resolves.
  The working set is session-only UI state — never persisted — because
  "what I had open yesterday" is not a good guess for "what I want tabs for
  today", and an unpersisted list needs no migration when destinations
  merge (issues #1457–#1460).
- **The ⊞ affordance opens the full destination list** as a menu of
  ADR-0031's tool-discovery groups — same tiles, labels, descriptions, and
  scope notes as the palette's start screen, so "tool grid" means one thing
  across the omnibar and the inspector. Selecting a tile switches
  destination and appends it to the working set. It follows the WAI-ARIA
  menu-button contract: click, ArrowDown (first item), or ArrowUp (last
  item) open it; Escape, focusout, and mousedown-outside close it.
- **Keyboard contracts:** the strip follows the WAI-ARIA tabs pattern
  (roving tabindex, Arrow/Home/End over stable positions, automatic
  activation); the menu follows the menu pattern (arrows move focus through
  ADR-0031's virtual 2-column grid via `toolDiscoveryArrowTarget`,
  Enter/click activates, Esc closes and restores focus to the trigger).
- **The header keeps its job.** The inspector header remains the context
  surface (lens, subject, following/pinned mode, pin control); the rail is
  navigation only and deliberately carries no context information.
- **Narrow widths degrade by column, not by truncation.** Below 320px panel
  width the tab labels collapse to icons (names remain in `title` and
  `aria-label`), and the tool menu drops to a single column — a 2-column
  grid truncates tile names to a few characters at the dock's 240px
  minimum (`probe-ui-checklist.md` §2: 240px is the case to design for).

## Consequences

- The inspector gains ~36px of chrome while open; the body scroller loses
  the same. Tabs are `shrink-0` siblings of the scroller per
  `probe-ui-checklist.md` §1, and the rail introduces no second scroller.
- `uiStore` gains `probeWorkingSet` (`{ tabs, mru }`; updated only via
  `pushProbeWorkingSet`). Tests that drive destinations must reset it.
  `ProbeTab` moved to `lib/probeContext.ts` (re-exported from `uiStore`)
  so pure domain modules never import from `stores/`.
- The cap keeps the working set honest, but favourites-pinning (a stable
  tab that never evicts) is the known follow-up if eviction surprises
  users.
- The ADR-0030 reopen surfaces (palette, title bar, contextual entries) are
  unchanged; the rail adds no reopen affordance and renders nothing while
  the inspector is closed.
