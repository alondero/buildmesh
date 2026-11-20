/**
 * Navigation continuity — restoring the saved scope on relaunch (issue #2793).
 *
 * The problem
 * -----------
 * The View Mode was persisted on its own (`buildmesh.view-mode`), without the
 * subject it scoped to. A returning user got the mode back and an empty
 * canvas: the header read "Mesh Grid / no mesh selected" while their agents
 * were still in the sidebar. The workspace looked lost even though nothing
 * was gone.
 *
 * The shape of the fix
 * --------------------
 * `navigationSnapshot.ts` owns the stored record. This module owns everything
 * around it:
 *
 *   1. `planNavigationRestore` — a PURE function that resolves the saved
 *      record against the authoritative Mesh and Agent Node snapshots and
 *      decides what to restore, what was lost, and where the canvas lands
 *      when something is gone. It reads no store and changes nothing, so the
 *      rules can be pinned by unit tests.
 *   2. `restoreNavigation` — applies the plan through the EXISTING navigation
 *      intents (`enterMesh` / `focusNode` / `setViewMode`), so a restored scope
 *      cannot disagree with a scope the user reached by clicking.
 *   3. `installNavigationSnapshotPersistence` — records the live navigation
 *      position as the user moves, so the next launch has something to
 *      restore.
 *
 * Three rules the plan keeps
 * --------------------------
 *   * **Restore only against authoritative snapshots.** The restore runs after
 *     the boot loaders have loaded the Mesh and Agent Node snapshots, so a
 *     saved id is resolved against what the backend actually has — never
 *     against a half-loaded list.
 *   * **Live navigation wins.** A user who navigates while the boot is still
 *     loading has made a newer decision than the saved record. The restore
 *     compares the live position with the position the boot started from and
 *     steps aside when they differ.
 *   * **Never guess a subject.** A saved Mesh that was deleted is not
 *     replaced with some other repository, and a saved Agent Node that was
 *     deleted or archived is not replaced with the first node that happens to
 *     be there. The canvas lands in a view that is valid and useful, and the
 *     loss is announced.
 *
 * The restore never launches, resumes or duplicates a process: it moves the
 * canvas, nothing else.
 */

import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useMeshStore } from '../stores/meshStore';
import { useUIStore, type NonSingleViewMode, type ViewMode } from '../stores/uiStore';
import { addToast } from '../stores/toastStore';
import { focusNode } from './navigation';
import { runBoot, type BootLoaders, type BootOutcome } from './bootSequence';
import {
  matchesGridControls,
  scopeNodesForMode,
  type FilterControls,
} from './viewModes';
import {
  SCOPE_NOTICE_PROVIDER,
  restoredMeshMissingNotice,
  restoredNodeArchivedNotice,
  restoredNodeMissingNotice,
  viewModeLabel,
} from './scopeNotices';
import {
  NAVIGATION_SNAPSHOT_VERSION,
  readNavigationSnapshot,
  writeNavigationSnapshot,
  type NavigationSnapshot,
} from './navigationSnapshot';
import type { AgentNode } from '../types/generated/AgentNode';
import type { Mesh } from '../types/generated/Mesh';

/** The live navigation position: what the canvas is showing right now. */
export interface NavigationState {
  viewMode: ViewMode;
  gridMode: NonSingleViewMode;
  selectedMeshId: number | null;
  activeNodeId: number | null;
}

/** The authoritative snapshots the saved record is resolved against. */
export interface NavigationWorld {
  meshes: readonly Mesh[];
  nodes: readonly AgentNode[];
  /** The active grid controls — the canvas only focuses a node it shows. */
  controls: FilterControls;
}

/** What was lost when the saved record pointed at something that is gone. */
export type NavigationRecovery =
  | { kind: 'mesh-missing'; meshId: number; meshName: string | null }
  | { kind: 'node-missing'; nodeId: number; nodeName: string | null }
  | { kind: 'node-archived'; nodeId: number; nodeName: string | null };

/** The resolved position to restore, plus what could not be restored. */
export interface NavigationRestorePlan {
  viewMode: ViewMode;
  gridMode: NonSingleViewMode;
  selectedMeshId: number | null;
  selectedMeshName: string | null;
  activeNodeId: number | null;
  activeNodeName: string | null;
  recoveries: NavigationRecovery[];
}

export type NavigationRestoreOutcome =
  /** The saved position was restored. */
  | 'restored'
  /** The saved position was restored, but part of it was gone. */
  | 'restored-with-recovery'
  /** The user navigated while the boot was loading; that navigation stands. */
  | 'skipped-live-navigation'
  /** There was no saved position to restore. */
  | 'skipped-no-snapshot';

export interface NavigationRestoreResult {
  outcome: NavigationRestoreOutcome;
  plan: NavigationRestorePlan | null;
}

/** Read the live navigation position from the three stores that own it. */
export function captureNavigationState(): NavigationState {
  return {
    viewMode: useUIStore.getState().viewMode,
    gridMode: useUIStore.getState().lastNonSingleMode,
    selectedMeshId: useMeshStore.getState().selectedMeshId,
    activeNodeId: useAgentNodeStore.getState().activeNodeId,
  };
}

function sameNavigationState(a: NavigationState, b: NavigationState): boolean {
  return a.viewMode === b.viewMode
    && a.gridMode === b.gridMode
    && a.selectedMeshId === b.selectedMeshId
    && a.activeNodeId === b.activeNodeId;
}

function gridControlsFrom(ui: {
  gridSearchQuery: string;
  gridProviderFilter: string | null;
  gridStatusFilter: FilterControls['gridStatusFilter'];
}): FilterControls {
  return {
    gridSearchQuery: ui.gridSearchQuery,
    gridProviderFilter: ui.gridProviderFilter,
    gridStatusFilter: ui.gridStatusFilter,
  };
}

/**
 * Whether the restored scope actually SHOWS this node.
 *
 * The canvas owns one rule here (ticket #986): in a grid mode the focused
 * node is always one of the visible nodes, so switching scope moves the focus
 * to the first visible node. The restore does not fight that rule — it only
 * restores a node the restored scope would show anyway. Single is exempt: it
 * derives FROM the focused node.
 */
function nodeIsInScope(
  gridMode: NonSingleViewMode,
  selectedMeshId: number | null,
  node: AgentNode,
  nodes: readonly AgentNode[],
  controls: FilterControls,
): boolean {
  const scoped = scopeNodesForMode(gridMode, nodes, selectedMeshId);
  return scoped.some(n => n.id === node.id) && matchesGridControls(node, controls);
}

/**
 * Resolve the saved record against the loaded snapshots.
 *
 * Pure: it reads the world it is given and returns the position to restore.
 * The rules, in order:
 *
 *   * A Mesh that is not in the snapshot is gone. Its id is dropped — never
 *     swapped for another Mesh — and the loss is recorded.
 *   * An Agent Node that is not in the snapshot is gone; one whose status is
 *     `archived` is no longer available either. Both are dropped and
 *     recorded. An archived node is not "missing": it still exists, so the
 *     notice says so.
 *   * Mesh Grid without a Mesh is the empty workspace this issue is about, so
 *     a Mesh Grid whose Mesh is gone lands in All Nodes instead.
 *   * Single whose node is gone lands in the grid Single was entered from —
 *     the scope the user was working in before the solo — and falls back to
 *     All Nodes when that grid was a Mesh Grid whose Mesh is gone too.
 *   * The All Nodes invariant (`viewMode === 'all'` ⟹ no Mesh selection) is
 *     applied last, so a restored All Nodes never carries a Mesh selection.
 */
export function planNavigationRestore(
  snapshot: NavigationSnapshot,
  world: NavigationWorld,
): NavigationRestorePlan {
  const recoveries: NavigationRecovery[] = [];

  // ---- the Mesh ----
  let selectedMeshId = snapshot.selectedMeshId;
  let selectedMeshName = snapshot.selectedMeshName;
  if (selectedMeshId !== null) {
    const mesh = world.meshes.find(m => m.id === selectedMeshId);
    if (mesh === undefined) {
      recoveries.push({ kind: 'mesh-missing', meshId: selectedMeshId, meshName: selectedMeshName });
      selectedMeshId = null;
    } else {
      selectedMeshName = mesh.name;
    }
  }

  // ---- the Agent Node ----
  let activeNodeId = snapshot.activeNodeId;
  let activeNodeName = snapshot.activeNodeName;
  let activeNode: AgentNode | null = null;
  if (activeNodeId !== null) {
    const node = world.nodes.find(n => n.id === activeNodeId) ?? null;
    if (node === null) {
      recoveries.push({ kind: 'node-missing', nodeId: activeNodeId, nodeName: activeNodeName });
      activeNodeId = null;
    } else if (node.status === 'archived') {
      recoveries.push({ kind: 'node-archived', nodeId: activeNodeId, nodeName: node.name });
      activeNodeId = null;
    } else {
      activeNode = node;
      activeNodeName = node.name;
    }
  }

  // ---- the View Mode ----
  let viewMode = snapshot.viewMode;
  let gridMode = snapshot.gridMode;
  if (viewMode === 'mesh' && selectedMeshId === null) {
    viewMode = 'all';
    gridMode = 'all';
  }
  if (viewMode === 'single' && activeNodeId === null) {
    // The soloed node is gone: return to the grid the solo was entered from.
    viewMode = gridMode;
    if (viewMode === 'mesh' && selectedMeshId === null) {
      viewMode = 'all';
      gridMode = 'all';
    }
  }

  // ---- the focused node against the restored scope ----
  // Single is exempt: it derives FROM the focused node rather than
  // constraining it.
  if (viewMode !== 'single' && activeNodeId !== null && activeNode !== null) {
    if (!nodeIsInScope(viewMode, selectedMeshId, activeNode, world.nodes, world.controls)) {
      // The node is real but outside the restored scope. The canvas's own
      // grid invariant moves the focus to the first visible node; the restore
      // leaves that to it rather than setting a focus it would take away.
      activeNodeId = null;
    }
  }

  // ---- the All Nodes invariant ----
  if (viewMode === 'all') {
    selectedMeshId = null;
    gridMode = 'all';
  }

  return {
    viewMode,
    gridMode,
    selectedMeshId,
    selectedMeshName,
    activeNodeId,
    activeNodeName,
    recoveries,
  };
}

/**
 * Apply the plan through the existing navigation intents, so a restored scope
 * is indistinguishable from one the user reached by clicking.
 *
 * The order is load-bearing:
 *   * `setViewMode('all')` clears the Mesh selection (the All Nodes
 *     invariant), so the Mesh selection is restored AFTER the mode that
 *     clears it.
 *   * `setViewMode('single')` keeps the current return target, so the grid
 *     mode Single returns to is established BEFORE the solo.
 */
function applyNavigationRestore(plan: NavigationRestorePlan): void {
  const ui = useUIStore.getState();
  const meshStore = useMeshStore.getState();

  if (plan.viewMode === 'single') {
    ui.setViewMode(plan.gridMode);
    if (plan.activeNodeId !== null) {
      // `scope: 'current'` keeps the restored Mesh: the solo must not follow
      // the node into another Mesh and undo the restored scope.
      focusNode(plan.activeNodeId, { view: 'single', scope: 'current' });
    } else {
      ui.setViewMode('single');
    }
    if (plan.selectedMeshId !== null) {
      meshStore.selectMesh(plan.selectedMeshId);
    }
    return;
  }

  if (plan.selectedMeshId !== null) {
    meshStore.selectMesh(plan.selectedMeshId);
  }
  if (plan.activeNodeId !== null) {
    focusNode(plan.activeNodeId, { view: 'current' });
  }
  ui.setViewMode(plan.viewMode);
}

/** Announce what could not be restored, and where the canvas landed. */
function announceRecovery(plan: NavigationRestorePlan): void {
  const landing = viewModeLabel(plan.viewMode);
  for (const recovery of plan.recoveries) {
    if (recovery.kind === 'mesh-missing') {
      addToast(SCOPE_NOTICE_PROVIDER, restoredMeshMissingNotice(recovery.meshName, landing), 'info');
    } else if (recovery.kind === 'node-missing') {
      addToast(SCOPE_NOTICE_PROVIDER, restoredNodeMissingNotice(recovery.nodeName, landing), 'info');
    } else {
      addToast(SCOPE_NOTICE_PROVIDER, restoredNodeArchivedNotice(recovery.nodeName, landing), 'info');
    }
  }
}

// Persistence

/**
 * Whether the live navigation may be recorded yet.
 *
 * The writer stays closed until the boot restore has run. A boot that has not
 * restored yet holds an EMPTY position (no Mesh selected, no node focused);
 * recording it would overwrite the saved subjects with the boot's own
 * defaults before they were ever restored — the exact data loss this module
 * exists to prevent.
 */
let persistenceOpen = false;
let unsubscribers: Array<() => void> = [];
let lastWritten: NavigationSnapshot | null = null;

/** The live position as a stored record. Ids that no longer exist are dropped. */
export function currentNavigationSnapshot(): NavigationSnapshot {
  const ui = useUIStore.getState();
  const meshes = useMeshStore.getState();
  const nodes = useAgentNodeStore.getState();
  const mesh = meshes.selectedMeshId === null
    ? undefined
    : meshes.meshesById.get(meshes.selectedMeshId);
  const node = nodes.activeNodeId === null
    ? undefined
    : nodes.nodesById[nodes.activeNodeId];
  return {
    version: NAVIGATION_SNAPSHOT_VERSION,
    viewMode: ui.viewMode,
    gridMode: ui.lastNonSingleMode,
    // The All Nodes invariant, applied to the RECORD as well as to the
    // store: a write that lands between `selectMesh` and the mode switch
    // that follows it would otherwise record All Nodes WITH a Mesh, and a
    // restore would then drop the Mesh the user had chosen.
    selectedMeshId: ui.viewMode === 'all' ? null : mesh?.id ?? null,
    selectedMeshName: ui.viewMode === 'all' ? null : mesh?.name ?? null,
    activeNodeId: node?.id ?? null,
    activeNodeName: node?.name ?? null,
  };
}

function sameSnapshot(a: NavigationSnapshot, b: NavigationSnapshot): boolean {
  return a.viewMode === b.viewMode
    && a.gridMode === b.gridMode
    && a.selectedMeshId === b.selectedMeshId
    && a.selectedMeshName === b.selectedMeshName
    && a.activeNodeId === b.activeNodeId
    && a.activeNodeName === b.activeNodeName;
}

function persistCurrentNavigationSnapshot(): void {
  const snapshot = currentNavigationSnapshot();
  if (lastWritten !== null && sameSnapshot(lastWritten, snapshot)) return;
  lastWritten = snapshot;
  writeNavigationSnapshot(snapshot);
}

/**
 * Record the live navigation position as the user moves.
 *
 * Installed once, at app start. Every store change reaches the writer; the
 * snapshot is rebuilt and compared, so only a real navigation change costs a
 * write.
 */
export function installNavigationSnapshotPersistence(): void {
  if (unsubscribers.length > 0) return;
  const write = (): void => {
    if (!persistenceOpen) return;
    persistCurrentNavigationSnapshot();
  };
  unsubscribers = [
    useMeshStore.subscribe(write),
    useAgentNodeStore.subscribe(write),
    useUIStore.subscribe(write),
  ];
}

/** Open the writer and record the position the canvas is showing now. */
function openSnapshotPersistence(): void {
  persistenceOpen = true;
  persistCurrentNavigationSnapshot();
}

/**
 * Restore the saved position.
 *
 * `baseline` is the navigation position the boot started from. When the live
 * position differs from it, the user navigated while the boot was loading —
 * that navigation is the newer truth, so the restore steps aside and only
 * records where the user left the canvas.
 */
export function restoreNavigation(baseline: NavigationState): NavigationRestoreResult {
  const live = captureNavigationState();
  if (!sameNavigationState(live, baseline)) {
    openSnapshotPersistence();
    return { outcome: 'skipped-live-navigation', plan: null };
  }

  const snapshot = readNavigationSnapshot();
  if (snapshot === null) {
    openSnapshotPersistence();
    return { outcome: 'skipped-no-snapshot', plan: null };
  }

  const plan = planNavigationRestore(snapshot, {
    meshes: useMeshStore.getState().meshes,
    nodes: useAgentNodeStore.getState().getAgentNodes(),
    controls: gridControlsFrom(useUIStore.getState()),
  });
  applyNavigationRestore(plan);
  announceRecovery(plan);
  openSnapshotPersistence();
  return {
    outcome: plan.recoveries.length > 0 ? 'restored-with-recovery' : 'restored',
    plan,
  };
}

/** Test-only: drop the writer and its state so each test starts clean. */
export function resetNavigationContinuityForTests(): void {
  for (const unsubscribe of unsubscribers) unsubscribe();
  unsubscribers = [];
  persistenceOpen = false;
  lastWritten = null;
}

/**
 * The boot, with the navigation restore as its last step.
 *
 * The restore runs INSIDE the boot, after every loader has settled. That is
 * the whole point: the saved ids are resolved against the authoritative
 * snapshots, so a slow or out-of-order load cannot be read as "the subject is
 * gone" — a Mesh whose snapshot is still in flight would otherwise be
 * announced as deleted and the scope would be dropped.
 *
 * A boot that did not produce an authoritative workspace restores nothing:
 * the caller shows the boot error and the saved position stays untouched for
 * the retry.
 */
export async function runBootWithRestore(
  loaders: BootLoaders,
  baseline: NavigationState,
): Promise<BootOutcome> {
  const outcome = await runBoot(loaders);
  if (!outcome.ok) return outcome;
  restoreNavigation(baseline);
  return outcome;
}
