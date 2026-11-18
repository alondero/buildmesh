/**
 * Navigation continuity (issue #2793) — restoring the saved Mesh, Agent Node
 * and View Mode on relaunch.
 *
 * The rules under test:
 *   * a saved position is restored against the AUTHORITATIVE snapshots, in
 *     every View Mode;
 *   * a subject that is gone is dropped and announced, never replaced with an
 *     unrelated one, and the canvas lands in a valid, useful view;
 *   * the restore launches nothing and resumes nothing;
 *   * navigation the user made while the boot was loading is not overwritten;
 *   * the live navigation is recorded, so the next launch has something to
 *     restore.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  captureNavigationState,
  currentNavigationSnapshot,
  installNavigationSnapshotPersistence,
  planNavigationRestore,
  resetNavigationContinuityForTests,
  restoreNavigation,
  runBootWithRestore,
  type NavigationWorld,
} from '../../src/lib/navigationContinuity';
import {
  NAVIGATION_SNAPSHOT_STORAGE_KEY,
  NAVIGATION_SNAPSHOT_VERSION,
  parseNavigationSnapshot,
  writeNavigationSnapshot,
  type NavigationSnapshot,
} from '../../src/lib/navigationSnapshot';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';

const BUILD_MESH = { id: 1, name: 'buildmesh' } as Mesh;
const PLAYGROUND_MESH = { id: 2, name: 'playground' } as Mesh;

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 30,
    mesh_id: 1,
    name: 'fix-terminal-blank',
    path: '/repo',
    branch: 'main',
    env: null,
    provider: 'claude',
    status: 'running',
    use_worktree: false,
    position: 0,
    created_at: '2026-01-01',
    scratchpad: '',
    sandbox: false,
    is_pinned: false,
    ...overrides,
  };
}

const NODES: AgentNode[] = [
  makeNode(),
  makeNode({ id: 31, mesh_id: 1, name: 'diff-panel-refactor', position: 1 }),
  makeNode({ id: 32, mesh_id: 2, name: 'scratch', position: 0 }),
];

const MESHES: Mesh[] = [BUILD_MESH, PLAYGROUND_MESH];

function world(overrides: Partial<NavigationWorld> = {}): NavigationWorld {
  return {
    meshes: MESHES,
    nodes: NODES,
    controls: { gridSearchQuery: '', gridProviderFilter: null, gridStatusFilter: null },
    ...overrides,
  };
}

function snapshot(overrides: Partial<NavigationSnapshot> = {}): NavigationSnapshot {
  return {
    version: NAVIGATION_SNAPSHOT_VERSION,
    viewMode: 'mesh',
    gridMode: 'mesh',
    selectedMeshId: 1,
    selectedMeshName: 'buildmesh',
    activeNodeId: 30,
    activeNodeName: 'fix-terminal-blank',
    ...overrides,
  };
}

/** The live navigation position, in the order the assertions read it. */
function live(): [number | null, number | null, string, string] {
  return [
    useMeshStore.getState().selectedMeshId,
    useAgentNodeStore.getState().activeNodeId,
    useUIStore.getState().viewMode,
    useUIStore.getState().lastNonSingleMode,
  ];
}

describe('navigation continuity', () => {
  beforeEach(() => {
    localStorage.clear();
    resetNavigationContinuityForTests();
    useMeshStore.setState({
      meshes: MESHES,
      meshesById: new Map(MESHES.map(m => [m.id, m])),
      selectedMeshId: null,
    });
    seedAgentNodes(NODES);
    useUIStore.setState({
      viewMode: 'all',
      lastNonSingleMode: 'all',
      gridSearchQuery: '',
      gridProviderFilter: null,
      gridStatusFilter: null,
    });
    useToastStore.setState({ toasts: [], notifications: [] });
  });

  describe('the plan (pure)', () => {
    it('restores the Mesh Grid scope with its Mesh and Agent Node', () => {
      const plan = planNavigationRestore(snapshot(), world());
      expect(plan).toMatchObject({
        viewMode: 'mesh',
        gridMode: 'mesh',
        selectedMeshId: 1,
        activeNodeId: 30,
        recoveries: [],
      });
    });

    it('restores Single with the node and the grid Single was entered from', () => {
      const plan = planNavigationRestore(
        snapshot({ viewMode: 'single', gridMode: 'mesh', activeNodeId: 31 }),
        world(),
      );
      expect(plan).toMatchObject({
        viewMode: 'single',
        gridMode: 'mesh',
        selectedMeshId: 1,
        activeNodeId: 31,
      });
    });

    it('restores Pinned with the Mesh selection and the pinned node', () => {
      const pinned = makeNode({ id: 33, mesh_id: 2, name: 'pinned-node', is_pinned: true });
      const plan = planNavigationRestore(
        snapshot({ viewMode: 'pinned', gridMode: 'pinned', selectedMeshId: 2, activeNodeId: 33 }),
        world({ nodes: [...NODES, pinned] }),
      );
      expect(plan).toMatchObject({
        viewMode: 'pinned',
        gridMode: 'pinned',
        selectedMeshId: 2,
        activeNodeId: 33,
      });
    });

    it('restores All Nodes without a Mesh selection (the All Nodes invariant)', () => {
      const plan = planNavigationRestore(
        snapshot({ viewMode: 'all', gridMode: 'all', selectedMeshId: null, activeNodeId: 32 }),
        world(),
      );
      expect(plan).toMatchObject({
        viewMode: 'all',
        gridMode: 'all',
        selectedMeshId: null,
        activeNodeId: 32,
      });
    });

    it('drops a deleted Mesh and lands in All Nodes instead of an empty Mesh Grid', () => {
      const plan = planNavigationRestore(
        snapshot({ selectedMeshId: 99, selectedMeshName: 'gone', activeNodeId: null }),
        world(),
      );
      expect(plan.viewMode).toBe('all');
      expect(plan.selectedMeshId).toBeNull();
      expect(plan.recoveries).toEqual([
        { kind: 'mesh-missing', meshId: 99, meshName: 'gone' },
      ]);
    });

    it('never replaces a deleted Mesh with another Mesh', () => {
      const plan = planNavigationRestore(
        snapshot({ selectedMeshId: 99, selectedMeshName: 'gone', activeNodeId: null }),
        world(),
      );
      expect(plan.selectedMeshId).not.toBe(1);
      expect(plan.selectedMeshId).not.toBe(2);
    });

    it('keeps the Mesh scope when only the Agent Node is gone', () => {
      const plan = planNavigationRestore(
        snapshot({ activeNodeId: 404, activeNodeName: 'closed' }),
        world(),
      );
      expect(plan).toMatchObject({
        viewMode: 'mesh',
        selectedMeshId: 1,
        activeNodeId: null,
      });
      expect(plan.recoveries).toEqual([
        { kind: 'node-missing', nodeId: 404, nodeName: 'closed' },
      ]);
    });

    it('treats an archived Agent Node as unavailable, not as missing', () => {
      const archived = makeNode({ id: 34, name: 'archived-node', status: 'archived' });
      const plan = planNavigationRestore(
        snapshot({ activeNodeId: 34, activeNodeName: 'archived-node' }),
        world({ nodes: [...NODES, archived] }),
      );
      expect(plan.activeNodeId).toBeNull();
      expect(plan.recoveries).toEqual([
        { kind: 'node-archived', nodeId: 34, nodeName: 'archived-node' },
      ]);
    });

    it('lands Single in the grid it was entered from when the soloed node is gone', () => {
      const plan = planNavigationRestore(
        snapshot({ viewMode: 'single', gridMode: 'mesh', activeNodeId: 404 }),
        world(),
      );
      expect(plan).toMatchObject({
        viewMode: 'mesh',
        gridMode: 'mesh',
        selectedMeshId: 1,
        activeNodeId: null,
      });
    });

    it('lands Single in All Nodes when the grid it came from lost its Mesh too', () => {
      const plan = planNavigationRestore(
        snapshot({ viewMode: 'single', gridMode: 'mesh', selectedMeshId: 99, activeNodeId: 404 }),
        world(),
      );
      expect(plan).toMatchObject({
        viewMode: 'all',
        gridMode: 'all',
        selectedMeshId: null,
        activeNodeId: null,
      });
    });

    it('does not restore a node the restored scope does not show', () => {
      // Node 32 lives in the playground Mesh, so it is not in the buildmesh
      // Mesh Grid. The canvas focuses a visible node in a grid mode, so the
      // restore must not set a focus the canvas would take away.
      const plan = planNavigationRestore(
        snapshot({ selectedMeshId: 1, activeNodeId: 32 }),
        world(),
      );
      expect(plan.selectedMeshId).toBe(1);
      expect(plan.activeNodeId).toBeNull();
    });

    it('does not restore a node the active filters hide', () => {
      const plan = planNavigationRestore(
        snapshot({ selectedMeshId: 1, activeNodeId: 30 }),
        world({ controls: { gridSearchQuery: 'nothing-matches', gridProviderFilter: null, gridStatusFilter: null } }),
      );
      expect(plan.activeNodeId).toBeNull();
    });

    it('restores a node the filters keep visible', () => {
      const plan = planNavigationRestore(
        snapshot({ selectedMeshId: 1, activeNodeId: 30 }),
        world({ controls: { gridSearchQuery: 'terminal', gridProviderFilter: null, gridStatusFilter: null } }),
      );
      expect(plan.activeNodeId).toBe(30);
    });
  });

  describe('the restore', () => {
    it('restores the selected Mesh and the active Agent Node', () => {
      writeNavigationSnapshot(snapshot());
      const result = restoreNavigation(captureNavigationState());
      expect(result.outcome).toBe('restored');
      expect(live()).toEqual([1, 30, 'mesh', 'mesh']);
    });

    it('restores a second Mesh with a non-first node', () => {
      writeNavigationSnapshot(snapshot({
        selectedMeshId: 2,
        selectedMeshName: 'playground',
        activeNodeId: 32,
        activeNodeName: 'scratch',
      }));
      restoreNavigation(captureNavigationState());
      expect(live()).toEqual([2, 32, 'mesh', 'mesh']);
    });

    it('restores Single with the node and keeps the grid it returns to', () => {
      writeNavigationSnapshot(snapshot({ viewMode: 'single', gridMode: 'mesh', activeNodeId: 31 }));
      restoreNavigation(captureNavigationState());
      expect(live()).toEqual([1, 31, 'single', 'mesh']);
    });

    it('restores Pinned with its Mesh selection', () => {
      const pinned = makeNode({ id: 33, mesh_id: 2, name: 'pinned-node', is_pinned: true });
      seedAgentNodes([...NODES, pinned]);
      writeNavigationSnapshot(snapshot({
        viewMode: 'pinned',
        gridMode: 'pinned',
        selectedMeshId: 2,
        selectedMeshName: 'playground',
        activeNodeId: 33,
        activeNodeName: 'pinned-node',
      }));
      restoreNavigation(captureNavigationState());
      expect(live()).toEqual([2, 33, 'pinned', 'pinned']);
    });

    it('restores All Nodes without a Mesh selection', () => {
      writeNavigationSnapshot(snapshot({
        viewMode: 'all',
        gridMode: 'all',
        selectedMeshId: null,
        activeNodeId: 32,
      }));
      restoreNavigation(captureNavigationState());
      expect(live()).toEqual([null, 32, 'all', 'all']);
    });

    it('lands in All Nodes and announces a deleted Mesh', () => {
      writeNavigationSnapshot(snapshot({
        selectedMeshId: 99,
        selectedMeshName: 'gone',
        activeNodeId: null,
      }));
      const result = restoreNavigation(captureNavigationState());
      expect(result.outcome).toBe('restored-with-recovery');
      expect(live()).toEqual([null, null, 'all', 'all']);
      const notices = useToastStore.getState().toasts;
      expect(notices).toHaveLength(1);
      expect(notices[0].provider).toBe('Scope');
      expect(notices[0].message).toContain('gone');
      expect(notices[0].message).toContain('All Nodes');
    });

    it('keeps the Mesh scope when the Agent Node was deleted', () => {
      writeNavigationSnapshot(snapshot({ activeNodeId: 404, activeNodeName: 'closed' }));
      const result = restoreNavigation(captureNavigationState());
      expect(result.outcome).toBe('restored-with-recovery');
      expect(live()).toEqual([1, null, 'mesh', 'mesh']);
      expect(useToastStore.getState().toasts[0].message).toContain('closed');
    });

    it('announces an archived Agent Node', () => {
      const archived = makeNode({ id: 34, name: 'archived-node', status: 'archived' });
      seedAgentNodes([...NODES, archived]);
      writeNavigationSnapshot(snapshot({ activeNodeId: 34, activeNodeName: 'archived-node' }));
      restoreNavigation(captureNavigationState());
      expect(live()).toEqual([1, null, 'mesh', 'mesh']);
      expect(useToastStore.getState().toasts[0].message).toContain('archived');
    });

    it('lands Single in the grid it came from when the soloed node was deleted', () => {
      writeNavigationSnapshot(snapshot({ viewMode: 'single', gridMode: 'mesh', activeNodeId: 404 }));
      const result = restoreNavigation(captureNavigationState());
      expect(result.outcome).toBe('restored-with-recovery');
      expect(live()).toEqual([1, null, 'mesh', 'mesh']);
    });

    it('starts from the app defaults when the stored record is corrupt', () => {
      localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, '{not json');
      const result = restoreNavigation(captureNavigationState());
      expect(result.outcome).toBe('skipped-no-snapshot');
      expect(live()).toEqual([null, null, 'all', 'all']);
    });

    it('starts from the app defaults when there is no stored record', () => {
      const result = restoreNavigation(captureNavigationState());
      expect(result.outcome).toBe('skipped-no-snapshot');
      expect(live()).toEqual([null, null, 'all', 'all']);
    });

    it('survives unavailable storage', () => {
      const getItem = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
        throw new Error('storage unavailable');
      });
      expect(() => restoreNavigation(captureNavigationState())).not.toThrow();
      getItem.mockRestore();
    });

    it('launches and resumes nothing', () => {
      const invokeMock = vi.mocked(invoke);
      invokeMock.mockClear();
      writeNavigationSnapshot(snapshot());
      restoreNavigation(captureNavigationState());
      expect(live()).toEqual([1, 30, 'mesh', 'mesh']);
      expect(invokeMock).not.toHaveBeenCalled();
    });

    it('does not overwrite navigation the user made while the boot was loading', () => {
      const baseline = captureNavigationState();
      // The user switches to Pinned before the snapshots finish loading.
      useUIStore.getState().setViewMode('pinned');
      useMeshStore.getState().selectMesh(2);
      writeNavigationSnapshot(snapshot());

      const result = restoreNavigation(baseline);
      expect(result.outcome).toBe('skipped-live-navigation');
      expect(live()).toEqual([2, null, 'pinned', 'pinned']);
    });
  });

  describe('the boot', () => {
    it('waits for a slow Mesh snapshot before resolving the saved Mesh', async () => {
      writeNavigationSnapshot(snapshot());
      const outcome = await runBootWithRestore({
        attachListeners: async () => {},
        // The Mesh snapshot lands after the Agent Node snapshot.
        loadMeshes: async () => {
          await new Promise(resolve => setTimeout(resolve, 20));
          useMeshStore.setState({
            meshes: MESHES,
            meshesById: new Map(MESHES.map(m => [m.id, m])),
          });
        },
        loadAgentNodes: async () => { seedAgentNodes(NODES); },
        readSnapshotErrors: () => ({ meshes: null, agentNodes: null }),
      }, captureNavigationState());

      expect(outcome.ok).toBe(true);
      expect(live()).toEqual([1, 30, 'mesh', 'mesh']);
      // A snapshot still in flight is not a deleted subject.
      expect(useToastStore.getState().toasts).toHaveLength(0);
    });

    it('restores nothing when the boot did not produce an authoritative workspace', async () => {
      writeNavigationSnapshot(snapshot());
      const outcome = await runBootWithRestore({
        attachListeners: async () => {},
        loadMeshes: async () => { throw new Error('mesh IPC died'); },
        loadAgentNodes: async () => { seedAgentNodes(NODES); },
        readSnapshotErrors: () => ({ meshes: 'mesh IPC died', agentNodes: null }),
      }, captureNavigationState());

      expect(outcome.ok).toBe(false);
      expect(live()).toEqual([null, null, 'all', 'all']);
      // The saved position survives for the retry.
      expect(parseNavigationSnapshot(localStorage.getItem(NAVIGATION_SNAPSHOT_STORAGE_KEY))).not.toBeNull();
    });
  });

  describe('the persistence', () => {    it('records the live navigation as the user moves', () => {
      installNavigationSnapshotPersistence();
      writeNavigationSnapshot(snapshot());
      restoreNavigation(captureNavigationState());

      useMeshStore.getState().selectMesh(2);
      useAgentNodeStore.getState().setActiveNode(32);

      const stored = parseNavigationSnapshot(localStorage.getItem(NAVIGATION_SNAPSHOT_STORAGE_KEY));
      expect(stored).toMatchObject({
        viewMode: 'mesh',
        selectedMeshId: 2,
        selectedMeshName: 'playground',
        activeNodeId: 32,
        activeNodeName: 'scratch',
      });
    });

    it('records the View Mode the user picks', () => {
      installNavigationSnapshotPersistence();
      writeNavigationSnapshot(snapshot());
      restoreNavigation(captureNavigationState());

      useUIStore.getState().setViewMode('pinned');

      const stored = parseNavigationSnapshot(localStorage.getItem(NAVIGATION_SNAPSHOT_STORAGE_KEY));
      expect(stored?.viewMode).toBe('pinned');
    });

    it('does not record anything before the restore has run', () => {
      installNavigationSnapshotPersistence();
      // A boot that has not restored yet holds an empty position. Recording
      // it would overwrite the saved subjects before they were restored.
      useMeshStore.getState().selectMesh(2);
      useAgentNodeStore.getState().setActiveNode(32);
      expect(localStorage.getItem(NAVIGATION_SNAPSHOT_STORAGE_KEY)).toBeNull();
    });

    it('drops an id that no longer exists', () => {
      installNavigationSnapshotPersistence();
      writeNavigationSnapshot(snapshot());
      restoreNavigation(captureNavigationState());

      seedAgentNodes(NODES.filter(n => n.id !== 30));

      const stored = parseNavigationSnapshot(localStorage.getItem(NAVIGATION_SNAPSHOT_STORAGE_KEY));
      expect(stored?.activeNodeId).toBeNull();
    });

    it('describes the live position from the stores', () => {
      // A Mesh-scoped position: All Nodes never records a Mesh (the
      // All Nodes invariant), so the live position is described in a mode
      // that can hold one.
      useUIStore.getState().setViewMode('mesh');
      useMeshStore.getState().selectMesh(2);
      useAgentNodeStore.getState().setActiveNode(32);
      expect(currentNavigationSnapshot()).toMatchObject({
        viewMode: 'mesh',
        gridMode: 'mesh',
        selectedMeshId: 2,
        selectedMeshName: 'playground',
        activeNodeId: 32,
        activeNodeName: 'scratch',
      });
    });

    it('never records a Mesh selection beside All Nodes (the All Nodes invariant)', () => {
      useUIStore.getState().setViewMode('all');
      useMeshStore.getState().selectMesh(2);
      expect(currentNavigationSnapshot()).toMatchObject({
        viewMode: 'all',
        selectedMeshId: null,
      });
    });
  });
});
