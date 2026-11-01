# 52. Probe scope uses progressive disclosure

Status: accepted (2026-10-31), following the user's request for two levels of
Probe header detail: destination and scope, with familiar affordances in place
of permanent explanatory text.

## Context and decision

The old header repeats Host as both a subject and a mode and reserves another
line for Following selection. The user's request replaces this hierarchy with
two lines: destination name, then current scope.

This partially supersedes [ADR-0029](0029-probe-context-lenses.md)'s requirement
to display Following selection permanently and its literal Host display label.
It also amends [ADR-0041](0041-disclosure-over-capture.md) sections 1–4: Host
renders as Host-wide; the visible Mesh or Agent subject and working-tree detail
carry current identity; the invariant following mode is disclosed on demand.
The ownership lenses, baselines, selection resolution, unavailable-context
behavior, removal of capture and possible future drift signal remain in force.

The scope line uses a disclosure chevron. Click, tap, Enter or Space opens full
scope names and mode text; Escape closes it. The summary's accessible name
includes the full scope and mode. Native title text supplements this disclosure
for hover users. The collapsed header keeps its two-line hierarchy at 240px.

## Alternatives and consequences

Keeping Following selection in the scope row repeats an invariant and competes
with actual identity at narrow widths. A hover-only tooltip hides information
from keyboard and touch users. The expandable scope retains concise chrome
while making the explanation available through each input method.

Selection remains live: changing a subject updates its label and closes stale
scope details. This changes presentation without migrating persisted state.
The disclosure is supplemental; the selected subject remains visible before
an action. Its complete name is available when narrow layout truncates it.

## Verification and current guidance

`tests/unit/probe-panel.test.tsx` covers scope naming, accessible disclosure and
Escape. `tests/integration/ui-shot-probe-header.steps.mjs` exercises all Probe
destinations at 240px, long Mesh and Agent names, keyboard/touch disclosure,
and focus changes between two nodes in one Mesh.

Current contracts: [design guidance](../../DESIGN.md#affordances-and-progressive-disclosure),
[Probe architecture](../development/probe-ui.md#probe-context-lenses-issue-1456),
and [user guide](../user-guide.md).
