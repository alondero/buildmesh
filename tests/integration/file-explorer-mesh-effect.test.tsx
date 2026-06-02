/**
 * Regression test for the "changes" header click doing nothing.
 *
 * Clicking the git-summary chip in an agent node header calls
 * toggleFileExplorer({ type: 'agent', ... }), which sets fileExplorerContext.
 * SessionView also has an effect that closes the file explorer when the
 * selected mesh changes. That effect must react ONLY to mesh switches —
 * if it also depends on fileExplorerContext it re-fires the instant an
 * agent panel is opened and closes it again, so (with a mesh selected)
 * nothing ever appears. See SessionView.tsx.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, act } from '@testing-library/react';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

// Heavy children are irrelevant to the effect under test — stub them out.
vi.mock('../../src/components/Terminal/Terminal', () => ({
  AgentTerminal: () => null,
  terminalManager: { fit: vi.fn(), fitAll: vi.fn() },
}));
vi.mock('../../src/components/Terminal/BuildRunTerminal', () => ({
  BuildRunTerminal: () => null,
}));
vi.mock('../../src/components/SessionView/GridNodeHeader', () => ({
  GridNodeHeader: () => null,
}));
vi.mock('../../src/components/SessionView/GridSplitter', () => ({
  GridSplitter: () => null,
}));
vi.mock('../../src/components/FileTree/FileExplorerPanel', () => ({
  FileExplorerPanel: () => <div data-testid="file-explorer-panel" />,
}));
vi.mock('../../src/lib/tauri', () => ({
  watchSession: vi.fn().mockResolvedValue(undefined),
  unwatchSession: vi.fn().mockResolvedValue(undefined),
}));

import { SessionView } from '../../src/components/SessionView/SessionView';

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

describe('SessionView mesh-change effect', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ fileExplorerContext: null });
  });

  it('keeps an agent file explorer open after it is toggled on (mesh selected)', () => {
    const { queryByTestId } = render(<SessionView />);

    expect(queryByTestId('file-explorer-panel')).toBeNull();

    act(() => {
      useUIStore.getState().toggleFileExplorer({ type: 'agent', nodeId: NODE.id, path: '/repo' });
    });

    // Before the fix the mesh-change effect re-fired on this context change and
    // immediately closed the panel — so it never showed.
    expect(useUIStore.getState().fileExplorerContext).not.toBeNull();
    expect(queryByTestId('file-explorer-panel')).not.toBeNull();
  });

  it('still closes the agent file explorer when the selected mesh changes', () => {
    render(<SessionView />);

    act(() => {
      useUIStore.getState().toggleFileExplorer({ type: 'agent', nodeId: NODE.id, path: '/repo' });
    });
    expect(useUIStore.getState().fileExplorerContext).not.toBeNull();

    act(() => {
      useMeshStore.setState({ selectedMeshId: 2 });
    });
    expect(useUIStore.getState().fileExplorerContext).toBeNull();
  });
});
