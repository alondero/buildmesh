/**
 * CenterDiffOverlay — issue #379. The full-height diff viewer that overlays the
 * center terminal grid when a changed file is clicked in the Probe.
 *
 * These tests cover the externally-observable behaviour the acceptance criteria
 * call out: the toolbar (path + parent node + "Back to Terminals"), the diff
 * fetch (the right baseline per `source`), the two close paths (Esc / button),
 * and the auto-close when the focused node or selected mesh changes.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { CenterDiffOverlay } from '../../src/components/AgentNodeView/CenterDiffOverlay';
import { useUIStore, type DiffContext } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import type { DiffResult } from '../../src/lib/tauri';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const MESH: Mesh = {
  id: 1,
  name: 'demo-mesh',
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
  name: 'fix-the-bug',
  path: '/repo/worktrees/fix-the-bug',
  branch: 'main',
  env: 'windows',
  provider: 'anthropic',
  status: 'running',
  use_worktree: true,
  position: 0,
  created_at: '2026-01-01',
};

const OTHER_NODE: AgentNode = { ...NODE, id: 9, name: 'other-agent' };

const DIFF: DiffResult = {
  files: [
    {
      path: 'src/app.ts',
      status: 'modified',
      old_path: null,
      additions: 1,
      deletions: 1,
      binary: false,
      hunks: [
        {
          old_start: 1,
          old_lines: 1,
          new_start: 1,
          new_lines: 1,
          old_highlighted: '',
          new_highlighted: '',
          lines: [
            { line_type: 'remove', content: 'old', old_num: 1, new_num: null },
            { line_type: 'add', content: 'new', old_num: null, new_num: 1 },
          ],
        },
      ],
    },
  ],
};

const BASE_CTX: DiffContext = {
  filePath: 'src/app.ts',
  rootPath: '/repo/worktrees/fix-the-bug',
  nodeId: 7,
  meshId: 1,
  source: 'base',
};

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'diff_node_file_against_base') return Promise.resolve(DIFF);
    if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
    return Promise.resolve({});
  });
}

describe('CenterDiffOverlay (#379)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
    useMeshStore.setState({
      meshes: [MESH],
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    seedAgentNodes([NODE, OTHER_NODE], NODE.id);
    useUIStore.setState({ probeOpen: true, probeTab: 'files', activeDiffFile: BASE_CTX });
  });

  it('shows the file path, parent node name, and a Back to Terminals button', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('fix-the-bug')).toBeTruthy();
    expect(screen.getByRole('button', { name: /back to terminals/i })).toBeTruthy();
  });

  it('fetches the since-branching diff for a base-source context', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('diff_node_file_against_base', {
        nodeId: 7,
        filePath: 'src/app.ts',
      });
    });
  });

  it('fetches the uncommitted diff for a head-source context', async () => {
    const headCtx: DiffContext = { ...BASE_CTX, nodeId: null, source: 'head', rootPath: '/repo' };
    render(<CenterDiffOverlay diff={headCtx} />);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('diff_file_against_head', {
        sessionPath: '/repo',
        filePath: 'src/app.ts',
      });
    });
  });

  it('falls back to the mesh name when the context has no node', async () => {
    const headCtx: DiffContext = { ...BASE_CTX, nodeId: null, source: 'head', rootPath: '/repo' };
    render(<CenterDiffOverlay diff={headCtx} />);
    expect(await screen.findByText('demo-mesh')).toBeTruthy();
  });

  it('closes via the Back to Terminals button', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    fireEvent.click(await screen.findByRole('button', { name: /back to terminals/i }));
    expect(useUIStore.getState().activeDiffFile).toBeNull();
  });

  it('closes when Escape is pressed', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByText('src/app.ts');
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(useUIStore.getState().activeDiffFile).toBeNull();
  });

  it('auto-closes when a different agent node is focused', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByText('src/app.ts');
    // User clicks a background node card â†’ activeNodeId changes away from the
    // diff's owner.
    useAgentNodeStore.setState({ activeNodeId: OTHER_NODE.id });
    await waitFor(() => {
      expect(useUIStore.getState().activeDiffFile).toBeNull();
    });
  });

  it('auto-closes when a different project is selected', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByText('src/app.ts');
    useMeshStore.setState({ selectedMeshId: 2 });
    await waitFor(() => {
      expect(useUIStore.getState().activeDiffFile).toBeNull();
    });
  });

  it('survives the diff source flipping baseâ†’pr on a live instance (Rules of Hooks, issue #803)', async () => {
    // The Probe stays interactive, so a user with a base/head diff open can
    // click a PR in the Probe — `activeDiffFile` flips source on the SAME
    // mounted overlay. Before #803, the head/base body declared more hooks
    // than the PR body, so this re-render threw "rendered fewer hooks than
    // during the previous render" and white-screened the workspace.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'diff_node_file_against_base') return Promise.resolve(DIFF);
      if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
      if (cmd === 'get_pr_files') return Promise.resolve([]);
      return Promise.resolve({});
    });
    const { rerender } = render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByText('src/app.ts');

    const prCtx: DiffContext = {
      ...BASE_CTX,
      source: 'pr',
      prNumber: 42,
      filePath: 'src/app.ts',
    };
    expect(() => rerender(<CenterDiffOverlay diff={prCtx} />)).not.toThrow();
    // The PR breadcrumb renders — the overlay swapped bodies cleanly.
    expect(await screen.findByText('PR #42')).toBeTruthy();
  });

  it('does NOT auto-close when an unrelated store value changes', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByText('src/app.ts');
    // Re-selecting the diff's own mesh, and keeping the same focused node, is
    // not a context change — the overlay must stay open.
    useMeshStore.setState({ selectedMeshId: MESH.id });
    useAgentNodeStore.setState({ activeNodeId: NODE.id });
    // Give any spurious effect a chance to fire.
    await new Promise((r) => setTimeout(r, 0));
    expect(useUIStore.getState().activeDiffFile).toEqual(BASE_CTX);
  });
});
