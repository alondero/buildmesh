// Issue #2076 — "scope changes announce themselves".
//
// The copy and the one predicate behind both notices, kept pure so the
// producers stay thin and the wording is asserted once (see
// `tests/unit/ui-store.test.ts`, `view-mode-switcher.test.tsx`).
//
// Notices are informational and auto-dismiss: they use the existing toast
// stack (`src/stores/toastStore.ts` — `TOAST_TTL_MS` expiry, `TOAST_MAX`
// cap, same-severity tokens), they need no acknowledgement, and they NEVER
// gate or discard the live path they describe. A notice says what happened;
// the Mesh switch, the mode flip and the search focus all still run.
//
// The predicate reads the DERIVED scope (`deriveScope`, #2071) rather than
// `selectedMeshId` on its own: Single reports the grid scope it was entered
// from, so a solo out of a Mesh is a Mesh-anchored scope too, and it is the
// scope — not the View Mode label — that decides whether a search escaped.

import { deriveScope } from './viewModes';
import type { NonSingleViewMode, ViewMode } from '../stores/uiStore';

/** The toast `provider` column for every scope notice — one origin label for
 *  "the scope moved", whatever moved it. */
export const SCOPE_NOTICE_PROVIDER = 'Scope';

/**
 * A search leaves a Mesh-scoped View Mode for the cross-Mesh Filtered view.
 * The results therefore span Meshes — a consequence of the search matching
 * Agent Node names only, which `scopeNodesForMode` records as a deliberate
 * constraint ("search inside one Mesh" is not expressible).
 */
export const SEARCH_ESCAPE_NOTICE =
  'Search results span every Mesh — the search matches Agent Node names, not one Mesh.';

/** Deleting the Mesh the user is looking at clears `selectedMeshId`, which the
 *  mesh→mode subscription turns into All Nodes. Named after what happened
 *  rather than after the deleted-slot bookkeeping behind it. */
export function meshDeletedScopeNotice(meshName: string): string {
  return `Mesh “${meshName}” was deleted — the canvas moved to All Nodes.`;
}

/** The scope inputs a notice may read — deliberately narrower than
 *  `ScopeInput`: `isMeshScoped` is a function of the grid mode and the
 *  selection alone, so no node list or Mesh list is needed to answer it. */
export interface ScopeNoticeInput {
  viewMode: ViewMode;
  lastNonSingleMode: NonSingleViewMode;
  selectedMeshId: number | null;
}

/**
 * The notice a search gesture must show, or null when nothing escaped.
 *
 * Null covers both no-op cases for free, because the Filtered view is
 * cross-Mesh by construction: re-clicking the Filtered segment (the
 * "get me to the search box" re-arm) and searching from All Nodes or Pinned
 * both report a cross-Mesh scope, so neither can be an escape. Callers read
 * this BEFORE the mode flip — after it, every scope is cross-Mesh and the
 * notice would always be lost.
 */
export function searchEscapeNotice(input: ScopeNoticeInput): string | null {
  const scope = deriveScope({
    viewMode: input.viewMode,
    lastNonSingleMode: input.lastNonSingleMode,
    // `isMeshScoped` reads `gridMode` and `selectedMeshId` only, so an empty
    // node list cannot change the answer — this notice names the scope, it
    // never reports a count.
    agentNodes: [],
    selectedMeshId: input.selectedMeshId,
    activeNodeId: null,
  });
  return scope.isMeshScoped ? SEARCH_ESCAPE_NOTICE : null;
}