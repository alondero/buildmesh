/**
 * The git-summary chip in the Agent Node header used to render all three
 * counts in one muted-grey span, so added / modified / deleted blurred into
 * a single "+3 ~2 -1" smudge that washed out against the mesh tint. Each
 * count now gets its own semantic colour (green / amber / red), zero counts
 * stay muted so the eye lands on the non-zero ones, and an open agent
 * file-explorer panel still overrides the whole chip to cyan to signal
 * which node owns the panel.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

// Irrelevant to chip styling — stub it.
vi.mock('../../src/components/BuildRun/BuildRunDropdown', () => ({
  BuildRunDropdown: () => null,
}));

// Each test sets what the chip will see by calling summaryMock.mockReturnValue(...).
const summaryMock = vi.fn();
vi.mock('../../src/hooks/useGitSummary', () => ({
  useGitSummary: () => ({ summary: summaryMock(), loading: false, refresh: vi.fn() }),
}));

import { GridNodeHeader } from '../../src/components/SessionView/GridNodeHeader';

const NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: false,
  created_at: new Date(0).toISOString(),
};

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
};

describe('GridNodeHeader git-summary chip', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ fileExplorerContext: null });
    summaryMock.mockReset();
  });

  it('colours added / modified / deleted counts distinctly so the diff pops', () => {
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-green-400');
    expect(getByText('~2').className).toContain('text-amber-400');
    expect(getByText('-1').className).toContain('text-red-400');
  });

  it('mutes zero counts so the eye is drawn to the non-zero changes', () => {
    summaryMock.mockReturnValue({ total: 3, added: 3, modified: 0, deleted: 0 });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-green-400');
    expect(getByText('~0').className).toContain('text-text-muted');
    expect(getByText('-0').className).toContain('text-text-muted');
  });

  it('overrides per-status colour with cyan when this node owns the agent file-explorer panel', () => {
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    useUIStore.setState({
      fileExplorerContext: { type: 'agent', nodeId: NODE.id, path: '/repo' },
    });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-cyan');
    expect(getByText('~2').className).toContain('text-accent-cyan');
    expect(getByText('-1').className).toContain('text-accent-cyan');
  });
});
