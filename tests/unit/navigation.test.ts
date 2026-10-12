import { beforeEach, describe, expect, it } from 'vitest';
import { enterMesh, focusNode, revealRun } from '../../src/lib/navigation';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { useUIStore, type ViewMode } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const NODE: AgentNode = {
  id: 900, mesh_id: 42, name: 'impl', path: '/repo', branch: 'main', env: null,
  provider: 'claude', status: 'running', use_worktree: false, position: 0,
  created_at: '2026-01-01', scratchpad: '', sandbox: false, is_pinned: false,
};

function tuple() {
  const ui = useUIStore.getState();
  return [useMeshStore.getState().selectedMeshId, useAgentNodeStore.getState().activeNodeId,
    ui.viewMode, ui.probeOpen, ui.probeTab, ui.pendingCircuitRunFocus];
}

describe('desktop navigation intents', () => {
  beforeEach(() => {
    seedAgentNodes([NODE]);
    useMeshStore.setState({ selectedMeshId: 7 });
    useNodeActivityStore.setState({ selections: {}, utilities: {}, groups: [] });
    useUIStore.setState({ viewMode: 'pinned', lastNonSingleMode: 'pinned',
      probeOpen: false, probeTab: 'files', pendingCircuitRunFocus: null });
  });

  it('focuses an activity within the current grid without changing its scope or Probe', () => {
    expect(focusNode(900)).toBe(true);
    expect(tuple()).toEqual([7, 900, 'pinned', false, 'files', null]);
    expect(useNodeActivityStore.getState().selections[900]).toEqual({ nodeId: 900, utility: false });
  });

  it('follows a node into its mesh and optionally opens its Probe destination', () => {
    focusNode(900, { view: 'mesh', probeTab: 'review' });
    expect(tuple()).toEqual([42, 900, 'mesh', true, 'review', null]);
  });

  it('keeps a cross-mesh grid when the focused node belongs to the selected mesh', () => {
    useMeshStore.setState({ selectedMeshId: 42 });
    focusNode(900, { view: 'mesh' });
    expect(tuple()).toEqual([42, 900, 'pinned', false, 'files', null]);
  });

  it('retargets Single without replacing the grid it will return to', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    focusNode(900, { view: 'mesh' });
    expect(tuple()).toEqual([7, 900, 'single', false, 'files', null]);
    useUIStore.getState().exitSingleMode();
    expect(tuple()).toEqual([7, 900, 'mesh', false, 'files', null]);
  });

  it('follows a created node into a different mesh even from Single', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    focusNode(900, { view: 'mesh', scope: 'node' });
    expect(tuple()).toEqual([42, 900, 'mesh', false, 'files', null]);
  });

  it('keeps Single when a created node belongs to the selected mesh', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    useMeshStore.setState({ selectedMeshId: 42 });
    focusNode(900, { view: 'mesh', scope: 'node' });
    expect(tuple()).toEqual([42, 900, 'single', false, 'files', null]);
  });

  it.each(['all', 'pinned', 'filtered', 'mesh'] as const)(
    'solos a card from %s and restores that grid on exit', (mode) => {
      useUIStore.getState().setViewMode(mode);
      const selectedMesh = useMeshStore.getState().selectedMeshId;
      focusNode(900, { view: 'single', scope: 'current' });
      expect(tuple()).toEqual([selectedMesh, 900, 'single', false, 'files', null]);
      expect(useUIStore.getState().lastNonSingleMode).toBe(mode);
      useUIStore.getState().exitSingleMode();
      expect(tuple()).toEqual([selectedMesh, 900, mode, false, 'files', null]);
    },
  );

  it('finder focus enters the owning mesh before soloing a node', () => {
    focusNode(900, { view: 'single' });
    expect(tuple()).toEqual([42, 900, 'single', false, 'files', null]);
    useUIStore.getState().exitSingleMode();
    expect(tuple()).toEqual([42, 900, 'mesh', false, 'files', null]);
  });

  it('selects grouped activity and utility state through the production activity store', () => {
    seedAgentNodes([NODE, { ...NODE, id: 901 }]);
    useNodeActivityStore.setState({ groups: [[900, 901]] });
    focusNode(901, { utility: true, utilityMode: 'terminal', probeTab: 'properties' });
    expect(tuple()).toEqual([7, 901, 'pinned', true, 'properties', null]);
    expect(useNodeActivityStore.getState().selections[900]).toEqual({ nodeId: 901, utility: true });
    expect(useNodeActivityStore.getState().utilities[901]).toBe('terminal');
  });

  it.each(['missing', 'archived'] as const)('does not partially navigate to a %s node', (target) => {
    if (target === 'archived') seedAgentNodes([{ ...NODE, status: 'archived' }]);
    const before = tuple();
    expect(focusNode(target === 'missing' ? 999 : 900, { view: 'single', probeTab: 'review' })).toBe(false);
    expect(tuple()).toEqual(before);
    expect(useNodeActivityStore.getState().selections).toEqual({});
  });

  it.each(['all', 'pinned', 'filtered', 'single', 'mesh'] as ViewMode[])(
    'entering the selected mesh from %s restores its grid and opens the requested Probe', (mode) => {
      useUIStore.setState({ viewMode: mode });
      enterMesh(7, { probeTab: 'worktrees' });
      expect(tuple()).toEqual([7, null, 'mesh', true, 'worktrees', null]);
    },
  );

  it('enters a different mesh without guessing a node or opening the Probe', () => {
    enterMesh(42);
    expect(tuple()).toEqual([42, null, 'mesh', false, 'files', null]);
  });

  it('enters a mesh with a focused node and destination even from Single', () => {
    useUIStore.setState({ viewMode: 'single' });
    expect(enterMesh(42, { nodeId: 900, probeTab: 'circuits' })).toBe(true);
    expect(tuple()).toEqual([42, 900, 'mesh', true, 'circuits', null]);
  });

  it('rejects a mismatched node before entering the mesh or opening its Probe', () => {
    const before = tuple();
    expect(enterMesh(7, { nodeId: 900, probeTab: 'circuits' })).toBe(false);
    expect(tuple()).toEqual(before);
  });

  it('reveals a run from Single in its owning mesh without changing the active node', () => {
    focusNode(900, { view: 'single' });
    revealRun(7, 123);
    expect(tuple()).toEqual([7, 900, 'mesh', true, 'circuits', 123]);
    useUIStore.getState().consumeCircuitRunFocus();
    revealRun(7, 123);
    expect(useUIStore.getState().pendingCircuitRunFocus).toBe(123);
  });
});
