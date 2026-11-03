// Issue #2076 — "scope changes announce themselves".
//
// The copy behind the scope notice, kept pure so the producer stays thin and
// the wording is asserted once (see `tests/unit/mesh-store.test.ts`).
//
// Notices are informational and auto-dismiss: they use the existing toast
// stack (`src/stores/toastStore.ts` — `TOAST_TTL_MS` expiry, `TOAST_MAX`
// cap, same-severity tokens), they need no acknowledgement, and they NEVER
// gate or discard the live path they describe. A notice says what happened;
// the delete it describes still happens and the canvas still moves.

/** The toast `provider` column for every scope notice — one origin label for
 *  "the scope moved", whatever moved it. */
export const SCOPE_NOTICE_PROVIDER = 'Scope';

/** Names the move to All Nodes after deleting the selected Mesh. */
export function meshDeletedScopeNotice(meshName: string): string {
  return `Mesh “${meshName}” was deleted — the canvas moved to All Nodes.`;
}
