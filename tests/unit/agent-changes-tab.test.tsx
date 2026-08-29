/**
 * AgentChangesTab — issue #376. The Probe Panel's 🔍 tab body.
 *
 * The tab deliberately loads only the base-relative changed-file list on
 * mount. Full hunks are fetched lazily by CenterDiffOverlay after a row is
 * clicked, so opening the panel does not render or highlight every change.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AgentChangesTab } from '../../src/components/Probe/AgentChangesTab';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { GitStatus } from '../../src/lib/tauri';
import { resetPathInvalidatedCacheForTests } from '../../src/lib/pathInvalidatedCache';

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'grid',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo/worktrees/agent-1',
  branch: 'main',
  env: 'windows',
  provider: 'anthropic',
  status: 'running',
  use_worktree: true,
  position: 0,
  created_at: '2026-01-01',
};

const FILES: GitStatus[] = [
  { path: 'src/app.ts', status: 'modified', additions: 2, deletions: 1 },
  { path: 'README.md', status: 'added', additions: 8, deletions: 0 },
];

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'node_changed_files') return Promise.resolve(FILES);
    return Promise.resolve({});
  });
}

describe('AgentChangesTab (#376)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
    useMeshStore.setState({
      meshes: [MESH],
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useUIStore.setState({ probeOpen: true, probeTab: 'review', activeDiffFile: null });
    resetPathInvalidatedCacheForTests();
  });

  it('renders changed-file titles and line counts without loading an initial full diff', async () => {
    render(<AgentChangesTab />);

    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('README.md')).toBeTruthy();
    const appRow = screen.getByRole('button', {
      name: /open src\/app\.ts in the center diff overlay/i,
    });
    expect(appRow.textContent).toContain('+2');
    expect(appRow.textContent).toContain('-1');
    expect(screen.getByRole('button', {
      name: /open readme\.md in the center diff overlay/i,
    }).textContent).toContain('+8');

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('node_changed_files', { nodeId: NODE.id });
    });
    expect(invoke).not.toHaveBeenCalledWith('diff_node_against_base', { nodeId: NODE.id });
    expect(screen.queryByText('File Tree')).toBeNull();
  });

  it('opens the selected file in the centre diff overlay with a base-source context (#379)', async () => {
    render(<AgentChangesTab />);

    const fileButton = await screen.findByRole('button', {
      name: /open src\/app\.ts in the center diff overlay/i,
    });
    fireEvent.click(fileButton);

    expect(useUIStore.getState().activeDiffFile).toEqual({
      filePath: 'src/app.ts',
      rootPath: NODE.path,
      nodeId: NODE.id,
      meshId: MESH.id,
      source: 'base',
    });
  });

  it('keeps the path header wired to the active node worktree', async () => {
    render(<AgentChangesTab />);

    const openButton = screen.getByRole('button', { name: /open in file explorer/i });
    expect(openButton.querySelector('svg')).toBeTruthy();

    fireEvent.click(openButton);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('open_in_file_manager', {
        path: NODE.path,
      });
    });
  });
});
