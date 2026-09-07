# Autopilot indicators on Agent Nodes

Status: Variant A selected and implemented.

## Goal

Make automated ownership visible wherever Agent Nodes are scanned, especially the canvas header and sidebar, while preserving the existing node lifecycle signal. A node with no Autopilot association renders no additional indicator.

The throwaway prototype compared three directions during design and has been
removed now that the production implementation is in place.

- `A` — Pilot light: shape-coded active, waiting, and done symbols.
- `B` — Edge rail: animated, dashed, and solid rails.
- `C` — Micro-label: `AUTO`, `WAIT`, and `DONE` tags.

The isolated entry was intentional: the existing desktop page requires Tauri
and live ledger rows, and could not guarantee all four comparison cases without
changing real runs. It mirrored the current shell and its 192px sidebar
minimum; the winning direction is now implemented in the real components.

## Presentation model

Autopilot ownership and the Agent Node lifecycle are separate dimensions. Keep the existing lifecycle dot and derive one small Autopilot presentation state alongside it.

| Presentation | Meaning | Legacy Autopilot source | Circuit source | Suggested accessible label |
|---|---|---|---|---|
| Active | Automation is driving the node now | `implementing`, `finishing`, or `suffix_pending` plus node `running` or `spawning` | run `running` plus node `running` or `spawning` | `Autopilot active` |
| Waiting | Automated ownership is still live but not currently driving the node | active run plus node `ready`, `awaiting_input`, `pending`, or `suspended` | run `pending` or `paused`, or `running` plus a non-running node lifecycle | `Autopilot waiting` |
| Done | Automated ownership reached a terminal outcome; the node remains for inspection | `completed` or `merged` | run `completed` | `Autopilot done` |
| None | No legacy run or Circuit ownership exists | no `autopilot_runs` row | no `CircuitAgentOwnership` | Render nothing |

`ready` means the Agent Node yielded cleanly; it does not by itself mean a person is needed. The shared phase can remain Waiting while its detail/tone distinguishes “between turns” from `awaiting_input` (“needs input”) and an explicit Circuit pause. `suspended` likewise stays Waiting, with detail derived from the full Agent Node (including `cli_session_id`) so crash recovery is not described as an approval gate.

Define every remaining lifecycle combination conservatively: an active ownership plus `idle` is Waiting; plus `error` is Waiting with an error tone and “needs attention”; plus `completed` is Waiting with a reconciliation detail until its ownership ledger becomes terminal. Archived Agent Nodes render no indicator and normally never reach these visible components. Unknown ownership states use an error/unknown treatment rather than silently claiming Active or Done.

Failure must not be painted as successful Done. Render terminal legacy or Circuit `failed` as `Autopilot needs attention` using the waiting shape and an error tone. A cancelled Circuit renders no compact indicator: automation was explicitly withdrawn, while the richer details menu/Probe retains the historical outcome. The chosen direction should therefore support a tone override without adding a fifth everyday category.

## Decision

Implement Variant A, the Pilot light. It survives narrow sidebar rows, communicates without consuming label width, and its pause/check shapes do not rely on colour alone. Keep the fuller run name and raw state in the existing details menu/Probe; this indicator should answer only “is automation driving this node, waiting, or finished?”

Reserve one fixed 14px ownership cell in every sidebar row and canvas header. For an unpiloted Agent Node the cell remains empty—there is still no visible Autopilot indicator—but the lifecycle dot, provider icon, and node name stay in the same vertical columns as piloted rows. Center the Pilot light inside that cell.

Variant C is the safest fallback if usability review finds the symbols too subtle. Variant B is visually quiet, but it is easiest to miss and has the weakest non-colour semantics.

## Implementation plan

1. Extend `list_circuit_agent_ownerships_inner` so a visible Circuit source Agent Node retains its latest run ownership after `completed`, `failed`, or `cancelled`; today only `pending`, `running`, and `paused` source runs are returned, which would make the indicator disappear instead of reaching Done. Prefer the newest run for that source, let an active run override older terminal history, retain the existing step-agent ownership behavior, and continue excluding archived Agent Nodes.
2. Add one pure frontend presentation seam, for example `src/lib/autopilotNodePresentation.ts`. Its input is the full Agent Node plus optional legacy `AutopilotRunState` and optional `CircuitAgentOwnership`; its output is `null` or `{ phase: 'active' | 'waiting' | 'done', tone: 'automation' | 'warning' | 'success' | 'error', label, detail }`. Keep legacy and Circuit mapping in this one place. Validate the Circuit ownership's existing string state at this boundary and handle unknown values explicitly; do not let ad hoc string comparisons spread into components.
3. Resolve precedence explicitly. Circuit ownership currently wins in `GridNodeHeader` details, so the helper should do the same if malformed data contains both sources. Do not add an `autopilot` column to `agent_nodes`; both existing satellite ledgers remain the source of truth.
4. Add a small shared `AutopilotNodeIndicator` component. It receives the derived presentation object and renders nothing for `null`, the Variant A Pilot light otherwise, a tooltip, and an accessible label. Animation must respect reduced-motion settings.
5. Mount the shared component in a fixed 14px ownership cell in `GridNodeHeader` and `Sidebar/NodeItem`. Keep that empty cell for unpiloted nodes so adjacent identity columns remain aligned, while rendering no actual indicator for `null`. Preserve the ordinary lifecycle status dot; Autopilot is not a replacement for Running, Awaiting input, Error, or Completed.
6. Keep the details menu richer. Replace the private `AUTOPILOT_PILL_STYLES` mapping with copy derived from the shared presentation helper, while retaining specific underlying labels such as wrap-up, suffix, PR ready, merged, circuit name, and run id.
7. Make event refresh symmetrical. Legacy lifecycle events already patch `autopilotStates`; update the `circuit-run-updated` listener to refresh or patch `circuitOwnerships` for `running` and `paused` as well as `pending`, `completed`, `failed`, and `cancelled`. This is required for Active → Waiting → Active → Done to update without a manual node reload. Give the async refresh an owner so an older response cannot overwrite newer event state. Combine Circuit ownership with the already-live Agent Node lifecycle rather than adding polling.
8. Remove the prototype route, component, and npm script when the winner is rewritten as production code. Preserve this specification and record the chosen variant and rationale on the implementation issue.

## Acceptance evidence

- Database-test that a Circuit source Agent Node retains the newest terminal ownership, a newer active run supersedes older history, step-agent ownership still survives terminal runs, and archived nodes remain absent.
- Unit-test the pure mapper for every Agent Node lifecycle crossed with relevant legacy/Circuit states, including Active, Waiting, Done, failure, cancelled (returns `null`), unknown ownership, malformed dual ownership, and no ownership.
- Component-test that no association produces no DOM indicator; every rendered indicator has the expected accessible name and tooltip; reduced motion does not remove meaning.
- Extend `grid-node-header.test.tsx` and add/extend sidebar NodeItem tests to prove both surfaces consume the same presentation state.
- Assert that piloted and unpiloted rows reserve the same ownership-cell width and keep their lifecycle/provider/name columns aligned.
- At the listener boundary, drive `circuit-run-updated` through running → paused → running → completed and assert the visible node changes Active → Waiting → Active → Done without remounting or a manual refresh. Resolve older/newer refreshes in both orders and cover rejection so stale or failed reads cannot regress the newest state.
- Browser-check the real desktop shell in dark and light themes, the 192px minimum sidebar, a compact canvas header, and high-density multi-node layouts. Confirm the indicator does not collide with restart/resume/delete controls.
- Run `scripts\check.ps1 all` so both frontend and Rust ownership-query tests execute, then run the repository's UI verification flow for real rendered geometry.
