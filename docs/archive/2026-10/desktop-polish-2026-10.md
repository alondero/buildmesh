# Desktop interaction and theme polish

Status: current. Follow-up to the [October desktop audit](desktop-ux-audit-2026-10.md)
and [issue #2003](https://github.com/alondero/buildmesh/issues/2003).

## Interaction contract

Settings selects tabs automatically with wrapping Up/Down and Home/End.
Only the selected tab is in the Tab sequence; Tab enters its labelled panel.
Panels remain mounted so drafts survive navigation. The shared Modal continues
to own visible-control traversal and the unsaved-edit confirmation sequence.

Sidebar colour, Resume, Restart, Close and provider disclosure controls have
explicit 24px targets. Their glyphs remain compact. Available recovery actions
and Close stay visible without hover. The existing event handlers, menu
shortcuts, drag handle and terminal ownership are retained.

## Theme inventory and action hierarchy

The inventory used searches for white-alpha hover fills and extra opacity on
neutral text across desktop TSX. Changes consume existing tokens from
[DESIGN.md](../../../DESIGN.md); no palette or status meaning changes.

| Surface | Treatment |
|---|---|
| App toast dismiss hover | `bg-bg-card-hover` replaces `white/10` |
| Settings and mesh form/dialog primary actions | Selection surface, primary text, medium weight and cyan border; neutral hover elevation |
| Settings Cancel/Back and coordinator Copy | Secondary text, neutral border and hover elevation |
| Notes and Project Settings placeholders | Full `text-muted`, without additional opacity |
| Probe field hints, inspector context examples, usage timestamps, telemetry hints and loading hints | Full neutral text tokens |
| Diff line numbers and Remote Access hints | Full neutral text tokens |
| Confirm/Exit dialogs | Already use neutral/status tokens; retain destructive treatment |

Remaining literal white ring paint belongs to the macOS titlebar traffic
lights, which use the platform palette. The zoom control's reduced text opacity
applies only while disabled. Inactive mesh swatches and closing rows also retain
their state cues. These are distinct from fading enabled essential text.
Broader status/accent contrast work remains tracked by
[#740](https://github.com/alondero/buildmesh/issues/740).

Primary form actions use a selection surface and cyan outline with primary
text, so the action boundary carries the accent and the label remains readable
in both themes. Supporting actions use neutral secondary controls. Destructive
and agent actions retain their semantic colours. The choice is documented in
the design system rather than applied indiscriminately to every button.

## Verification

`app-settings-navigation.test.tsx` exercises roving selection, panel links,
draft retention and dirty-banner focus restoration through the production
Settings/Modal boundary. Its navigation case fails on the pre-change Settings
implementation.

The `desktop polish` scenarios in `verify-smoke.spec.ts` exercise 900x600,
1280x800 and 1920x1080, dark/light themes, and normal/reduced motion. They measure
actual targets, accessible names/tooltips, recovery opacity without hover,
menu Escape focus return, panel fit, Tab order and dirty confirmation behavior.
These automated Chromium scenarios use mock IPC; they do not verify spawning.

Native WebView2 screenshots use an isolated `com.alond.buildmesh.gh2003.dev`
profile with real Tauri IPC and backend resources. Completed review fixtures
provide sidebar rows; native lifecycle events supply recovery presentations
without launching a provider. Comparable before/after screenshots live in
[the PR evidence directory](../../pr-screenshots/gh2003). Functional native checks
use the same viewport/theme/motion matrix; provider spawning and terminal
lifetime changes are outside this polish pass.

The initial baseline used the native binary built at `d8759681`. Final comparable
captures serve the production assets built from that detached baseline checkout
through CDP at the native origin, against the unchanged Rust backend. After
captures use the rebuilt native binary's assets. Provider accounts settle before
capturing the form, with the same unsaved example credentials in both versions.
Provider and sidebar pairs compare settled content. General Settings baseline
captures retain the loading banner; use the functional checks for navigation
and geometry rather than attributing that loading-state difference to this change.
Native logs contain fixture-related missing-repository and inactive-PTY resize
errors already reproduced in the original native baseline; no panic files were
created. These fixtures establish UI behavior, not provider execution.
