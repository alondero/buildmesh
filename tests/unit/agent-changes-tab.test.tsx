/**
 * AgentChangesTab — issue #376. The Probe Panel's ðŸ” tab body.
 *
 * The tab deliberately loads only the base-relative changed-file list on
 * mount. Full hunks are fetched lazily by CenterDiffOverlay after a row is
 * clicked, so opening the panel does not render or highlight every change.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { AgentChangesTab } from '../../src/components/Probe/AgentChangesTab';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { type AgentNode } from '../../src/stores/agentNodeStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { GitStatus } from '../../src/lib/tauri';
import { resetPathInvalidatedCacheForTests } from '../../src/lib/pathInvalidatedCache';
import { GIT_CHANGED } from '../../src/lib/events';
import { seedAgentNodes } from './helpers/seedAgentNodes';

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

const ZERO_DELTA_FILE: GitStatus = {
  path: 'empty.txt',
  status: 'deleted',
  additions: 0,
  deletions: 0,
};

function mockBackend(changedFiles: GitStatus[] | Error | Promise<GitStatus[]> = FILES) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'node_changed_files') {
      return changedFiles instanceof Error
        ? Promise.reject(changedFiles)
        : Promise.resolve(changedFiles);
    }
    return Promise.resolve({});
  });
}

function mockChangedFilesSequence(results: Array<GitStatus[] | Error>) {
  let request = 0;
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'node_changed_files') {
      const result = results[Math.min(request++, results.length - 1)];
      return result instanceof Error ? Promise.reject(result) : Promise.resolve(result);
    }
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
    seedAgentNodes([NODE], NODE.id );
    useUIStore.setState({ probeOpen: true, probeTab: 'review', activeDiffFile: null });
    resetPathInvalidatedCacheForTests();
  });

  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('renders changed-file titles and line counts without loading an initial full diff', async () => {
    render(<AgentChangesTab />);

    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('README.md')).toBeTruthy();
    const appRow = screen.getByRole('button', {
      name: /open src\/app\.ts \(modified, \+2, -1\) in the center diff overlay/i,
    });
    expect(appRow.textContent).toContain('+2');
    expect(appRow.textContent).toContain('-1');
    expect(screen.getByRole('button', {
      name: /open readme\.md \(added, \+8, -0\) in the center diff overlay/i,
    }).textContent).toContain('+8');
    expect(screen.getByTitle('Modified').textContent).toBe('M');
    expect(screen.getByTitle('Added').textContent).toBe('A');

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('node_changed_files', { nodeId: NODE.id });
    });
    expect(invoke).not.toHaveBeenCalledWith('diff_node_against_base', { nodeId: NODE.id });
    expect(screen.queryByText('File Tree')).toBeNull();
  });

  it('shows a loading state while the initial file-list request is pending', () => {
    const pending = new Promise<GitStatus[]>(() => {});
    mockBackend(pending);

    render(<AgentChangesTab />);

    expect(screen.getByRole('status').textContent).toContain('Loading changed files…');
  });

  it('shows the formatted first-load error when the file-list request fails', async () => {
    mockBackend(new Error('git status unavailable'));

    render(<AgentChangesTab />);

    expect(await screen.findByText('git status unavailable')).toBeTruthy();
    expect(screen.queryByText('No changes vs Base Ref')).toBeNull();
  });

  it('renders no clean-worktree summary header when there are no changed files', async () => {
    mockBackend([]);

    render(<AgentChangesTab />);

    expect(await screen.findByText('No changes vs Base Ref')).toBeTruthy();
    expect(screen.queryByText(/0 files changed/)).toBeNull();
  });

  it('calculates summary additions and deletions and keeps zero-delta status visible', async () => {
    mockBackend([...FILES, ZERO_DELTA_FILE]);

    render(<AgentChangesTab />);

    expect(await screen.findByText('empty.txt')).toBeTruthy();
    expect(screen.getByText('+10')).toBeTruthy();
    const summary = screen.getByText(/3 files changed/).parentElement;
    expect(summary?.textContent).toContain('-1');
    expect(screen.getByTitle('Deleted').textContent).toBe('D');
    expect(screen.getByRole('button', {
      name: /open empty\.txt \(deleted, \+0, -0\) in the center diff overlay/i,
    })).toBeTruthy();
  });

  it('opens the selected file in the centre diff overlay with a base-source context (#379)', async () => {
    render(<AgentChangesTab />);

    const fileButton = await screen.findByRole('button', {
      name: /open src\/app\.ts \(modified, \+2, -1\) in the center diff overlay/i,
    });
    fireEvent.click(fileButton);

    expect(useUIStore.getState().activeDiffFile).toEqual({
      filePath: 'src/app.ts',
      rootPath: NODE.path,
      nodeId: NODE.id,
      meshId: MESH.id,
      source: 'base',
    });
    await waitFor(() => expect(fileButton.className).toContain('bg-bg-overlay'));
  });

  it('shows the stale list with a warning when a background refresh fails', async () => {
    mockChangedFilesSequence([FILES, new Error('temporary git failure')]);
    render(<AgentChangesTab />);

    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    vi.useFakeTimers({ now: Date.now() });

    await act(async () => {
      await emit(GIT_CHANGED, { path: NODE.path });
      await vi.advanceTimersByTimeAsync(2_000);
    });

    expect(screen.getByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('Refresh failed — showing last known changes')).toBeTruthy();
  });

  it('throttles GIT_CHANGED refetches during an agent edit burst (#1165)', async () => {
    render(<AgentChangesTab />);

    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    vi.useFakeTimers({ now: Date.now() });
    const callsBeforeBurst = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'node_changed_files'
    ).length;

    await act(async () => {
      // Move outside the initial fetch's freshness window so the first event
      // is the leading refetch; the remaining four events form the burst.
      vi.advanceTimersByTime(2_001);
      for (let i = 0; i < 5; i++) {
        await emit(GIT_CHANGED, { path: NODE.path });
        await vi.advanceTimersByTimeAsync(100);
      }
    });

    const callsAfterBurst = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'node_changed_files'
    ).length;
    expect(callsAfterBurst).toBeLessThanOrEqual(callsBeforeBurst + 1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_500);
    });

    const finalCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'node_changed_files'
    ).length;
    expect(finalCalls).toBeLessThanOrEqual(callsBeforeBurst + 2);
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
