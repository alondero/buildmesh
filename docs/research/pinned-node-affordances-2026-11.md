# Showing pinned nodes in the All Nodes view

Research dated 2026-11-18 for Buildmesh desktop contributors. The comparison uses
repository behavior at `c1bbae1e` and first-party product documentation. The
trade-offs are design judgments based on those sources.

## Recommendation

Show a small, filled pushpin beside a pinned node's name in the desktop canvas
and sidebar. Use the pushpin shape already associated with **Pinned** and
**Pin node**, and expose **Pinned** to assistive technology. A marker without a
click action fits in the existing row or title bar and lets users scan **All
Nodes** in their chosen order. The existing menus provide the pin action.
[View mode switcher](../../src/components/ViewModeSwitcher/ViewModeSwitcher.tsx),
[Card header](../../src/components/AgentNodeView/GridNodeHeader.tsx),
[Sidebar node row](../../src/components/Sidebar/NodeItem.tsx),
[Buildmesh design principles](../../DESIGN.md#principles).

## Approaches

| Approach | What it gives the user | Fit for All Nodes |
| --- | --- | --- |
| Filled pushpin beside the name | A visible cue on the item in its current position. Microsoft Teams uses an inline symbol to show that a conversation is muted. [Microsoft Teams chat guidance](https://support.microsoft.com/en-us/teams/chat/hide-unhide-mute-add-a-chat-to-favorites-or-mark-a-chat-as-unread-in-microsoft-teams). | Best fit. The existing silhouette ties the cue to the **Pinned** view and the menu action. A 12px glyph leaves most of the row for the node name. [View mode switcher](../../src/components/ViewModeSwitcher/ViewModeSwitcher.tsx), [Node card chrome](../../DESIGN.md#node-card-chrome). |
| **Pinned** text chip | An explicit label that a new user can read immediately. Carbon's read-only tags are an example of metadata shown beside items. [Carbon tag guidelines](https://www.carbondesignsystem.com/building-blocks/core/components/tag/guidelines). | Readable but wider on every pinned item. In a narrow card it competes with the name and chips that identify PRs or conditions needing attention. [Pills, badges, chips](../../DESIGN.md#pills-badges-chips). |
| Pinned-first ordering or a section | Quick access at a predictable edge. Firefox moves pinned tabs to the left and compresses them to site icons. Teams puts favorited chats in their own section. [Firefox pinned tabs](https://support.mozilla.org/en-US/kb/pinned-tabs-keep-favorite-websites-open), [Microsoft Teams chat guidance](https://support.microsoft.com/en-us/teams/chat/hide-unhide-mute-add-a-chat-to-favorites-or-mark-a-chat-as-unread-in-microsoft-teams). | Useful for a dedicated destination, which Buildmesh already provides in **Pinned**. Moving a card on pin would interrupt the spatial order users set with **Custom** and give pin priority over the chosen sort. [View mode scope](../../src/lib/viewModes.ts), [Grid sorting](../../src/components/AgentNodeView/gridFilterSort.ts), [Group nodes into tabs](../user-guide.md#group-nodes-into-tabs). |
| Border, tint, or status-glyph ring | An emphasis cue across the card or within an existing icon. [Node card](../../src/components/AgentNodeView/NodeCard.tsx), [Buildmesh status design](../../DESIGN.md#status). | The card border already carries selection and attention, the header tint identifies the Mesh, and the status glyph's orbit belongs to Circuit state. A pin silhouette provides a distinct cue without relying on colour. [Card header](../../src/components/AgentNodeView/GridNodeHeader.tsx), [WCAG 1.4.1](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html). |
| Always-visible pin toggle | Direct pinning and unpinning. A labelled, pressed-state button can communicate both the affordance and the current value. [WAI-ARIA button pattern](https://www.w3.org/WAI/ARIA/apg/patterns/button/). | Adds a focus stop and a 24px-wide target to each card or row, including unpinned items. The task is to find pinned items while scanning. [Buildmesh design principles](../../DESIGN.md#principles), [Card header](../../src/components/AgentNodeView/GridNodeHeader.tsx), [Sidebar node row](../../src/components/Sidebar/NodeItem.tsx). |

## Placement and contrast

- Read each marker from that Agent Node's `is_pinned` value. On a grouped card,
  the title names the root and the menu acts on the selected Agent Node. The
  title marker therefore belongs to the root. Member markers belong on the
  corresponding Agent Node tabs, where **Pinned** can join the existing
  accessible label and tooltip. Utility and Circuit Overview tabs describe
  activities with their own labels. Sidebar cluster members each have their own
  row. [Node card](../../src/components/AgentNodeView/NodeCard.tsx),
  [Card header](../../src/components/AgentNodeView/GridNodeHeader.tsx),
  [Activity tabs](../../src/components/AgentNodeView/NodeActivityTabs.tsx),
  [Sidebar cluster](../../src/components/Sidebar/NodeCluster.tsx).
- Keep the marker visible beside truncated names. A filled silhouette provides
  the visual distinction, and a text equivalent conveys the state in the
  accessibility tree. WCAG sets a 3:1 minimum contrast ratio against adjacent
  colours for meaningful graphics. [WCAG 1.4.1](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html),
  [WCAG 1.4.11](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html).
- A secondary-text pin over a card header's 8% Mesh tint has a minimum estimated
  contrast of 5.69:1 in dark and 5.84:1 in light when composited over `bg-card`
  with black and white custom tints. Sidebar hover uses a 60% Mesh tint. The
  same pin falls to about 1.5:1 over default amber in dark mode using a
  `bg-card` backing. A small opaque neutral surface behind the pin can restore
  contrast on those rows, including custom Mesh colours. These figures use
  sRGB compositing of the source tokens and the WCAG luminance formula.
  [Theme tokens](../../src/App.css), [Mesh palette](../../src/lib/meshColors.ts),
  [Card header](../../src/components/AgentNodeView/GridNodeHeader.tsx),
  [Sidebar row](../../src/components/Sidebar/NodeItem.tsx),
  [WCAG contrast formula](https://www.w3.org/TR/WCAG22/#dfn-contrast-ratio).
