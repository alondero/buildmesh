# 27. Circuit Canvas Editor on React Flow (positions stay out of the blueprint)

Status: accepted

The Autopilot Circuits authoring surface (#1209) is a full-screen
[@xyflow/react](https://xyflow.com) canvas with [@dagrejs/dagre] for auto-layout.
Two new runtime dependencies ride this decision: `@xyflow/react` and
`@dagrejs/dagre`.

## Context

Milestone 4 of the Autopilot Circuits spec (#1205) replaces the Probe tab's
throwaway create-form with a visual editor: palette, drag-to-wire, gate outcome
ports, editable edge conditions, live run telemetry, and Dagre LR/TB
auto-layout. Building that by hand over SVG would mean re-implementing zoom,
pan, node dragging, handle hit-testing, and edge geometry — weeks of work that
React Flow provides for free.

CLAUDE.md's rule is "no new deps beyond the task". Here the dependency *is* the
task, but its weight (a canvas framework) is the kind of adoption earlier ADRs
(e.g. 0010 rejecting `tauri-specta`) chose to record deliberately rather than
let land silently.

## Decision

- **Adopt `@xyflow/react` + `@dagrejs/dagre`.** Custom node/edge components
  (`CircuitNodeCard`, `OutcomeEdge`) keep the app's Tailwind token styling;
  React Flow supplies only the canvas mechanics.
- **The blueprint AST stays canonical in Rust.** `graph_json` never stores
  layout. Node positions are derived (Dagre on open / on demand) and live only
  for the editing session. Consequence: a user's dragged layout is lost when
  the editor closes — accepted, because persisting positions would either
  bloat the engine-consumed AST or fork a second "layout_json" surface nobody
  asked for yet.
- **The working copy lives as React Flow nodes/edges; the AST is *derived* at
  save time** (`toGraph` in `circuitGraphModel.ts`) and validated server-side
  (`update_circuit_graph` re-parses before persisting). An editor bug can
  never persist a graph the stepper can't read.
- **Controlled flow discipline:** the editor owns its nodes/edges state and
  passes them as props. Custom edges therefore mutate through an
  editor-provided callback (`data.onCycle`) instead of `useReactFlow().setEdges`
  — in a fully controlled flow the internal store is overwritten from props on
  every render, so direct store writes silently vanish.

## Considered options

- **Hand-rolled SVG/canvas.** No dependency, total control. Rejected: the spec
  explicitly names React Flow, and the mechanics listed above are exactly the
  parts that are expensive to get right.
- **Persist positions inside `graph_json` (extra serde field).** Keeps layout
  across sessions for free. Rejected: couples the engine's canonical AST to
  presentation state and forces a version bump on every layout tweak.
- **Separate `layout_json` column.** Honest split, but schema churn +
  migration for a UX nicety no acceptance criterion demands. Revisit if users
  actually complain about lost layouts.

## Consequences

- Bundle grows by React Flow + dagre (~150 kB gzipped); acceptable for a
  desktop Tauri app.
- Testing React Flow under vitest/jsdom needs polyfills (ResizeObserver that
  reports real dimensions, DOMMatrixReadOnly) — see
  `docs/learning/react-flow-vitest-jsdom.md`.
