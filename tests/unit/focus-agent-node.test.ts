import { describe, it, expect, beforeEach } from 'vitest';
import { focusAgentNode } from '../../src/lib/focusAgentNode';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const NODE: AgentNode = {
  id: 900, mesh_id: 42, name: 'impl', path: '/repo', branch: 'main', env: null,
  provider: 'claude', status: 'running', use_worktree: false, position: 0,
  created_at: '2026-01-01', scratchpad: '', sandbox: false, is_pinned: false,
};

describe('focusAgentNode', () => {
  beforeEach(() => {
    seedAgentNodes([]);
    useMeshStore.setState({ selectedMeshId: null });
    useNodeActivityStore.setState({ selections: {}, utilities: {} });
    useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
  });

  it('selects the mesh, activates the node, and solos it', () => {
    seedAgentNodes([NODE]);
    expect(focusAgentNode(900)).toBe(true);
    expect(useMeshStore.getState().selectedMeshId).toBe(42);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(900);
    expect(useNodeActivityStore.getState().selections[900]?.nodeId).toBe(900);
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('returns false and changes nothing when the node no longer exists', () => {
    expect(focusAgentNode(900)).toBe(false);
    expect(useMeshStore.getState().selectedMeshId).toBeNull();
    expect(useAgentNodeStore.getState().activeNodeId).toBeNull();
    expect(useNodeActivityStore.getState().selections).toEqual({});
    expect(useUIStore.getState().viewMode).toBe('all');
  });
});
