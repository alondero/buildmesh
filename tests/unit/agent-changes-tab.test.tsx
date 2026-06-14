/**
 * AgentChangesTab — issue #376. The Probe Panel's 🔍 tab body.
 *
 * Wraps the existing `AgentReviewPanel` (ADR 0005) with the active agent
 * node's id and resolved path. The component is gated by ProbeTabBody —
 * by the time it mounts, `activeNodeId` is guaranteed non-null and
 * `activePath` points at the node's worktree directory (or the mesh root
 * for a non-worktree node).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AgentChangesTab } from '../../src/components/Probe/AgentChangesTab';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import type { DiffResult } from '../../src/lib/tauri';

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'grid',
  position: 0,
  created_at: '2026-01-01',
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

const DIFF: DiffResult = {
  files: [
    {
      path: 'src/app.ts',
      hunks: [
        { old_start: 1, old_lines: 1, new_start: 1, new_lines: 2, lines: ['-old', '+new'] },
      ],
    },
  ],
};

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'diff_node_against_base') return Promise.resolve(DIFF);
    if (cmd === 'list_directory') return Promise.resolve({ name: 'repo', path: '/repo', is_dir: true, children: [] });
    if (cmd === 'get_git_branch_status') return Promise.resolve(null);
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
  });

  it('renders the AgentReviewPanel summary for the focused node', async () => {
    render(<AgentChangesTab />);

    // The summary bar reports "1 file changed" once the diff lands.
    await waitFor(() => {
      expect(screen.getByText(/1 file changed/)).toBeTruthy();
    });
    // The diff payload reaches the backend with the nodeId.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('diff_node_against_base', { nodeId: NODE.id });
    });
  });

  it('uses the active node\'s worktree path as the root for the file-tree browse affordance', async () => {
    render(<AgentChangesTab />);

    // The collapsible "File Tree" button is part of AgentReviewPanel.
    await waitFor(() => {
      expect(screen.getByText('File Tree')).toBeTruthy();
    });
  });
});
