/**
 * The navigation snapshot — the saved "where was I" record (issue #2793).
 *
 * Why a separate record
 * ---------------------
 * The View Mode alone was persisted (`buildmesh.view-mode`), and it was
 * persisted *without* its subject: the mode was restored on its own while the
 * Mesh it scoped to and the Agent Node it focused were dropped. A returning
 * user got the mode back and an empty canvas — the workspace read as lost
 * even though every agent was still there.
 *
 * The snapshot is one small, VERSIONED record of the whole navigation
 * position: the View Mode, the grid mode Single was entered from, the selected
 * Mesh and the active Agent Node. It is deliberately separate from process and
 * session lifecycle state (#1538 owns startup readiness, PTY restoration and CLI
 * session recovery): this record says where the user was looking, never what
 * should be running. Restoring it must not launch, resume or duplicate a
 * process.
 *
 * Why the names are stored
 * -------------------------
 * The ids are the identity; the names are only ever used to NAME a loss in a
 * recovery notice ("Mesh “buildmesh” no longer exists"). A deleted Mesh is no
 * longer in the loaded list, so without the saved name the notice could only
 * say "Mesh #12". The names are labels, never a scope: nothing resolves a
 * Mesh or a node by name.
 *
 * Storage rules
 * -------------
 * `localStorage`, try/catch-wrapped like `src/lib/theme.ts` and the View Mode
 * persistence in `uiStore`: an unavailable or full store must never break the
 * navigation it records. A record that cannot be parsed, or that was written
 * by a build with a different version, is ignored — the app then starts from
 * its own defaults and rewrites the record.
 *
 * The View Mode is ALSO persisted on its own (`buildmesh.view-mode`), because
 * the store needs a mode before the first paint: the snapshot can only be
 * resolved once the Mesh and Agent Node snapshots have loaded, while the mode
 * is a plain string the store can read at creation. The two records are
 * written from the same state and cannot disagree; the snapshot is the whole
 * position, and the mode key is its synchronous seed.
 */

import type { NonSingleViewMode, ViewMode } from '../stores/uiStore';

export const NAVIGATION_SNAPSHOT_VERSION = 1;
export const NAVIGATION_SNAPSHOT_STORAGE_KEY = 'buildmesh.navigation-snapshot';

/**
 * The saved navigation position. `version` gates the shape: a record written
 * by another version is discarded rather than half-understood.
 */
export interface NavigationSnapshot {
  version: number;
  /** The View Mode the canvas was showing. */
  viewMode: ViewMode;
  /** The grid mode Single was entered from — Single's Escape target. */
  gridMode: NonSingleViewMode;
  /** The selected Mesh id, or null when no Mesh was selected. */
  selectedMeshId: number | null;
  /** The selected Mesh's name at save time — a label for recovery notices. */
  selectedMeshName: string | null;
  /** The active Agent Node id, or null when no node was focused. */
  activeNodeId: number | null;
  /** The active Agent Node's name at save time — a label for recovery notices. */
  activeNodeName: string | null;
}

const VIEW_MODES: readonly ViewMode[] = ['single', 'mesh', 'pinned', 'all'];
const GRID_MODES: readonly NonSingleViewMode[] = ['mesh', 'pinned', 'all'];

/**
 * A stored id: absent (null) or a positive safe integer. `undefined` means the
 * stored value was malformed, which invalidates the whole record — a record
 * that half-parsed would restore a subject the user never chose.
 */
function storedId(value: unknown): number | null | undefined {
  if (value === null || value === undefined) return null;
  if (typeof value === 'number' && Number.isSafeInteger(value) && value > 0) return value;
  return undefined;
}

/** A stored label: absent (null) or a string. `undefined` means malformed. */
function storedName(value: unknown): string | null | undefined {
  if (value === null || value === undefined) return null;
  if (typeof value === 'string') return value;
  return undefined;
}

/**
 * Parse one stored record. Returns null for anything that cannot be trusted:
 * malformed JSON, a foreign shape, a version this build does not know, or a
 * field that does not type-check. The caller then starts from the app's own
 * defaults — a lost position is better than a wrong one.
 */
export function parseNavigationSnapshot(raw: string | null): NavigationSnapshot | null {
  if (raw === null) return null;

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== 'object' || parsed === null) return null;

  const record = parsed as Record<string, unknown>;
  if (record.version !== NAVIGATION_SNAPSHOT_VERSION) return null;

  const viewMode = record.viewMode;
  if (typeof viewMode !== 'string' || !VIEW_MODES.includes(viewMode as ViewMode)) return null;

  const gridMode = record.gridMode;
  const selectedMeshId = storedId(record.selectedMeshId);
  const activeNodeId = storedId(record.activeNodeId);
  const selectedMeshName = storedName(record.selectedMeshName);
  const activeNodeName = storedName(record.activeNodeName);
  if (selectedMeshId === undefined || activeNodeId === undefined) return null;
  if (selectedMeshName === undefined || activeNodeName === undefined) return null;

  return {
    version: NAVIGATION_SNAPSHOT_VERSION,
    viewMode: viewMode as ViewMode,
    // A record written before the grid mode was tracked (or a hand-edited
    // one) falls back to the neutral return target rather than failing the
    // whole record: the mode and the subjects are the parts that matter.
    gridMode: typeof gridMode === 'string' && GRID_MODES.includes(gridMode as NonSingleViewMode)
      ? gridMode as NonSingleViewMode
      : 'all',
    selectedMeshId,
    selectedMeshName,
    activeNodeId,
    activeNodeName,
  };
}

/** Read the saved position, or null when there is none to restore. */
export function readNavigationSnapshot(): NavigationSnapshot | null {
  try {
    return parseNavigationSnapshot(localStorage.getItem(NAVIGATION_SNAPSHOT_STORAGE_KEY));
  } catch {
    // Storage unavailable (private mode, a locked profile): nothing saved.
    return null;
  }
}

/** Write the saved position. A storage failure never blocks the navigation. */
export function writeNavigationSnapshot(snapshot: NavigationSnapshot): void {
  try {
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, JSON.stringify(snapshot));
  } catch {
    // Storage unavailable — the in-memory navigation still works.
  }
}
