# 30. Title-bar navigation and the on-demand Probe inspector

## Status

Accepted for the Probe navigation model (2026-09-02). Supersedes the
"future grouped rail" consequence of
[ADR-0029](0029-probe-context-lenses.md); the lens contract and destination
mapping from ADR-0029 remain fully in force.

## Context

The Probe dock's always-visible activity rail — one icon per destination,
11 deep — took permanent width from the workspace, made icon-only
destinations easy to confuse, and presented the internal Host/Mesh/Agent
groupings as if they were user tasks. Issue #1375 decided (after comparing
five prototype variants on `prototype/issue-1375-titlebar-inspector`) that
Probe should be an **on-demand inspector** reached through title-bar-first
navigation, not persistent navigation. The internal lens contract stays:
it governs data ownership, loading, and destructive-action safety — it just
must not be the primary navigation language.

## Decision

- **No persistent Probe navigation.** `ProbePanel` renders only while
  `probeOpen` is true; closed leaves no rail, sidebar, or always-visible
  button. Reopening happens through the palette, the title bar, and
  contextual entries (sidebar menus, node headers, issue/PR deep links).
- **A labelled "Search or open…" title-bar field opens the command
  palette** — the primary way to find destinations, commands, nodes, and
  GitHub items. Its kbd hint is read from `shortcutCatalog.ts` (the
  `open-omnibar` row), keeping the cheatsheet and the title bar on one
  display-label source.
- **Usage is a labelled title-bar action**, not a rail peer: it is a
  high-frequency, host-global utility. It is still served by the inspector
  shell as the host-lens `usage` destination (so it opens without a mesh)
  until #1461 moves it to a dedicated global surface.
- **The palette carries every destination under user-facing task names**
  (`APP_COMMANDS` `probe-*` entries; `PROBE_TAB_COMMANDS` aliases
  `PROBE_TAB_ORDER`, so the catalog cannot drift from the destination
  vocabulary). Command ids keep the stable `probe-<tab>` shape and
  `openProbeTab` is unchanged for compatibility with existing callers and
  deep links.
- **One name per destination.** `PROBE_TAB_DEFINITIONS[..].label` (the
  inspector header) must match the palette label of the same destination
  (Project Settings, GitHub Issues, Agent History, Notes, …). User-facing
  copy follows the issue's task language; internal vocabulary (Mesh, Agent
  Node) stays in code and comments per `CONTEXT.md`.

## Consequences

- Destination headers and palette entries must be renamed together; the
  "label ≠ header" mismatch class is a review finding.
- Follow-ups that reshape destinations on top of this model: #1457–#1460
  (merging Files/Agent Changes, Issues/PRs, Autopilot/Circuits; splitting
  Project settings from repository maintenance), #1461 (Usage as a global
  surface), #1462–#1464. The push-vs-overlay question at narrow widths and
  palette/node-filter unification remain open design questions from #1375.
- ADR-0029's "future grouped rail" consequence is superseded by this ADR;
  its lens table and `probeContext.ts` contract are not.
