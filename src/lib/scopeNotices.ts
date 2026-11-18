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
//
// Issue #2793 adds the restore notices: the same vocabulary, used when a
// relaunch cannot put the canvas back where the user left it because the
// subject they were working in is gone. The notice names what was lost and
// says which scope the canvas shows instead — the user is never moved to a
// scope they did not choose, and never left wondering why the canvas
// changed.

import type { ViewMode } from '../stores/uiStore';

/** The toast `provider` column for every scope notice — one origin label for
 * "the scope moved", whatever moved it. */
export const SCOPE_NOTICE_PROVIDER = 'Scope';

/** Names the move to All Nodes after deleting the selected Mesh. */
export function meshDeletedScopeNotice(meshName: string): string {
  return `Mesh “${meshName}” was deleted — the canvas moved to All Nodes.`;
}

/** The scope the canvas shows, in the words the View Mode switcher uses. */
export function viewModeLabel(mode: ViewMode): string {
  switch (mode) {
    case 'single': return 'Single';
    case 'mesh': return 'Mesh Grid';
    case 'pinned': return 'Pinned';
    case 'all': return 'All Nodes';
  }
}

/**
 * The Mesh a saved scope pointed at no longer exists (issue #2793).
 *
 * The canvas lands in a valid, useful scope rather than a Mesh Grid with no
 * Mesh selected, and the notice points at the one control that chooses a
 * scope — the scope label in the title bar — so the recovery is the user's
 * decision, not a silent substitution.
 */
export function restoredMeshMissingNotice(meshName: string | null, landing: string): string {
  const subject = meshName ? `Mesh “${meshName}”` : 'The Mesh you were working in';
  return `${subject} no longer exists — the canvas shows ${landing}. Choose another Mesh from the scope label in the title bar.`;
}

/** The Agent Node a saved scope pointed at no longer exists (issue #2793). */
export function restoredNodeMissingNotice(nodeName: string | null, landing: string): string {
  const subject = nodeName ? `Agent Node “${nodeName}”` : 'The Agent Node you were working on';
  return `${subject} no longer exists — the canvas shows ${landing}.`;
}

/** The Agent Node a saved scope pointed at was archived (issue #2793). */
export function restoredNodeArchivedNotice(nodeName: string | null, landing: string): string {
  const subject = nodeName ? `Agent Node “${nodeName}”` : 'The Agent Node you were working on';
  return `${subject} was archived — the canvas shows ${landing}.`;
}
