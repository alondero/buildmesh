/**
 * ProjectFilesTab — issue #376. The Probe Panel's ðŸ“ tab body.
 *
 * Mirrors the non-agent branch of the legacy `FileExplorerPanel`:
 * `ChangedFilesSection` on top, a collapsible `FileTree` underneath. A click
 * on a changed file should switch the probe to the `review` tab via the
 * existing `openDiff` action (the review tab is where the diff is consumed,
 * not here in the narrow 360px body).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ProjectFilesTab } from '../../src/components/Probe/ProjectFilesTab';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import type { FileNode, GitStatus, DiffResult } from '../../src/lib/tauri';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { useToastStore } from '../../src/stores/toastStore';

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
  { path: 'src/app.ts', status: 'modified', additions: 3, deletions: 1 },
];

const DIFF: DiffResult = { files: [{ path: 'src/app.ts', hunks: [] }] };

const TREE: FileNode = {
  name: 'repo',
  path: '/repo',
  is_dir: true,
  children: [
    { name: 'app.ts', path: '/repo/app.ts', is_dir: false, children: [] },
  ],
};

/** Route invoke() so both the git status and the file tree can be controlled. */
function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'get_git_status') return Promise.resolve(FILES);
    if (cmd === 'list_directory') return Promise.resolve(TREE);
    if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
    if (cmd === 'get_git_branch_status') return Promise.resolve(null);
    return Promise.resolve({});
  });
}

describe('ProjectFilesTab (#376)', () => {
  beforeEach(() => {
    useToastStore.setState({toasts:[]});
    vi.mocked(invoke).mockReset();
    mockBackend();
    useMeshStore.setState({
      meshes: [MESH],
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    seedAgentNodes([NODE], null);
    useUIStore.setState({
      probeOpen: true,
      probeTab: 'files',
      activeDiffFile: null,
    });
  });

  it('renders both the Changed Files and File Tree sections anchored on the active mesh path', async () => {
    render(<ProjectFilesTab />);

    // Changed Files section shows the git-status file. The shared cache
    // dedupes the fetch with useMeshGitStatus, but the component still
    // makes the get_git_status call once.
    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('Changed Files')).toBeTruthy();
    expect(screen.getByText('File Tree')).toBeTruthy();
  });

  it('collapses the File Tree when its header is clicked', async () => {
    render(<ProjectFilesTab />);

    await screen.findByRole('treeitem', { name: /app.ts/ });
    fireEvent.click(screen.getByText('File Tree'));
    // The Changed Files section is unaffected — only the tree sub-tree hides.
    expect(screen.getByText('Changed Files')).toBeTruthy();
  });

  it('opens changed tree files in the diff and unchanged files in the editor', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_git_status') return Promise.resolve([{ path: 'app.ts', status: 'modified', additions: 3, deletions: 1 }]);
      if (cmd === 'list_directory') return Promise.resolve({ ...TREE, children: [
        { name: 'app.ts', path: '/repo/app.ts', is_dir: false, children: [] },
        { name: 'readme.md', path: '/repo/readme.md', is_dir: false, children: [] },
      ] });
      if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
      return Promise.resolve(null);
    });
    render(<ProjectFilesTab />);
    fireEvent.click(await screen.findByRole('treeitem', { name: /app.ts/ }));
    await waitFor(() => expect(useUIStore.getState().activeDiffFile).toMatchObject({ filePath: 'app.ts', rootPath: '/repo', source: 'head' }));
    expect(invoke).toHaveBeenCalledWith('diff_file_against_head', {sessionPath:'/repo',filePath:'app.ts'});
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === 'open_in_editor')).toBe(false);
    fireEvent.click(screen.getByRole('treeitem', { name: /readme.md/ }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('open_in_editor', {path:'/repo/readme.md'}));
  });

  it('does not open a diff or editor when the changed tree file diff fails', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_git_status') return Promise.resolve([{ path: 'app.ts', status: 'modified', additions: 1, deletions: 0 }]);
      if (cmd === 'list_directory') return Promise.resolve(TREE);
      if (cmd === 'diff_file_against_head') return Promise.reject(new Error('Diff unavailable'));
      return Promise.resolve(null);
    });
    render(<ProjectFilesTab />);
    fireEvent.click(await screen.findByRole('treeitem', { name: /app.ts/ }));
    await waitFor(() => expect(useToastStore.getState().toasts.some(t=>t.message.includes('Diff unavailable'))).toBe(true));
    expect(useUIStore.getState().activeDiffFile).toBeNull();
    expect(vi.mocked(invoke).mock.calls.some(([cmd])=>cmd==='open_in_editor')).toBe(false);
  });

  function pendingTreeDiffs() {
    const pending = new Map<string, { resolve: (diff: DiffResult) => void; reject: (error: Error) => void }>();
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'get_git_status') return Promise.resolve(['app.ts', 'other.ts'].map(path => ({ path, status: 'modified', additions: 1, deletions: 0 })));
      if (cmd === 'list_directory') return Promise.resolve({ ...TREE, children: ['app.ts', 'other.ts'].map(name => ({ name, path: `/repo/${name}`, is_dir: false, children: [] })) });
      if (cmd === 'diff_file_against_head') return new Promise<DiffResult>((resolve, reject) => { pending.set(args!.filePath as string, { resolve, reject }); });
      return Promise.resolve(null);
    });
    return pending;
  }

  it.each(['older-first', 'newer-first', 'older-rejects'])(
    'keeps the latest changed-file selection when loads complete %s', async order => {
      const pending = pendingTreeDiffs();
      render(<ProjectFilesTab />);
      fireEvent.click(await screen.findByRole('treeitem', { name: /app.ts/ }));
      fireEvent.click(screen.getByRole('treeitem', { name: /other.ts/ }));
      if (order === 'older-first') {
        await act(async () => { pending.get('app.ts')!.resolve(DIFF); });
        expect(useUIStore.getState().activeDiffFile).toBeNull();
      }
      await act(async () => { pending.get('other.ts')!.resolve(DIFF); });
      expect(useUIStore.getState().activeDiffFile?.filePath).toBe('other.ts');
      await act(async () => {
        if (order === 'older-rejects') pending.get('app.ts')!.reject(new Error('Stale failure'));
        else pending.get('app.ts')!.resolve(DIFF);
      });
      expect(useUIStore.getState().activeDiffFile?.filePath).toBe('other.ts');
      expect(useToastStore.getState().toasts).toHaveLength(0);
    },
  );

  it.each(['collapse', 'unmount', 'owner-change'])(
    'does not reopen a diff after the tree leaves its owner by %s', async departure => {
      const pending = pendingTreeDiffs();
      const { unmount } = render(<ProjectFilesTab />);
      fireEvent.click(await screen.findByRole('treeitem', { name: /app.ts/ }));
      if (departure === 'collapse') fireEvent.click(screen.getByText('File Tree'));
      else if (departure === 'unmount') unmount();
      else act(() => { useAgentNodeStore.getState().setActiveNode(NODE.id); });
      await act(async () => { pending.get('app.ts')!.resolve(DIFF); });
      expect(useUIStore.getState().activeDiffFile).toBeNull();
    },
  );

  it.each(['tree-first', 'list-first'])(
    'shares latest-selection ownership across list and tree (%s)', async order => {
      const pending = pendingTreeDiffs();
      render(<ProjectFilesTab />);
      await screen.findByRole('treeitem', { name: /app.ts/ });
      if (order === 'tree-first') {
        fireEvent.click(screen.getByRole('treeitem', { name: /app.ts/ }));
        fireEvent.click(screen.getByRole('button', { name: /other.ts/ }));
      } else {
        fireEvent.click(screen.getByRole('button', { name: /app.ts/ }));
        fireEvent.click(screen.getByRole('treeitem', { name: /other.ts/ }));
      }
      await act(async () => { pending.get('other.ts')!.resolve(DIFF); });
      expect(useUIStore.getState().activeDiffFile?.filePath).toBe('other.ts');
      await act(async () => { pending.get('app.ts')!.resolve(DIFF); });
      expect(useUIStore.getState().activeDiffFile?.filePath).toBe('other.ts');
    },
  );

  it('clicking a changed file opens the center diff overlay via openDiff (#379)', async () => {
    render(<ProjectFilesTab />);

    const fileButton = await screen.findByText('src/app.ts');
    fireEvent.click(fileButton);

    await waitFor(() => {
      const state = useUIStore.getState();
      // The overlay context is a 'head'-source diff anchored on the active
      // mesh path (no node focused in this fixture â†’ nodeId null).
      expect(state.activeDiffFile).toEqual({
        filePath: 'src/app.ts',
        rootPath: '/repo',
        nodeId: null,
        meshId: 1,
        source: 'head',
      });
      // The Probe stays on the files tab so the list keeps responding to
      // clicks — the diff is consumed in the center, not the review tab.
      expect(state.probeTab).toBe('files');
      expect(state.probeOpen).toBe(true);
    });
  });

  // Regression: the open-in-OS-file-manager button + folder-open icon were
  // dropped from the FileExplorerPanel when it was deleted in ed0f8bc.
  // Pin both the visible affordance and the Tauri call so a future
  // refactor can't silently lose them again.
  it('renders an "Open in file explorer" button that calls open_in_file_manager with the active path', async () => {
    render(<ProjectFilesTab />);

    // The header strip shows the active path as the button's title text —
    // the legacy `FileExplorerPanel` rendered the same path as its
    // header title (no node focused here â†’ activePath is the mesh root).
    await screen.findByText('Changed Files');
    expect(screen.getByText('/repo')).toBeTruthy();

    const openButton = screen.getByRole('button', { name: /open in file explorer/i });
    // The button must render an SVG icon (Lucide folder-open) — pin that
    // an `<svg>` lives inside the button so the glyph can't be silently
    // replaced with text/emoji again.
    expect(openButton.querySelector('svg')).toBeTruthy();

    fireEvent.click(openButton);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('open_in_file_manager', {
        path: '/repo',
      });
    });
  });

  it('uses the focused node\'s worktree path when one is active', async () => {
    // The legacy panel used `context.path` which, for an agent context,
    // resolved to the node's worktree dir — same as today's
    // `activePath` when a node is focused. Pin that the button follows
    // the same path semantics so opening Explorer lands the user in
    // the worktree, not the mesh root.
    seedAgentNodes([NODE], NODE.id);

    render(<ProjectFilesTab />);

    await screen.findByText('Changed Files');
    expect(screen.getByText(NODE.path)).toBeTruthy();

    const openButton = screen.getByRole('button', { name: /open in file explorer/i });
    fireEvent.click(openButton);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('open_in_file_manager', {
        path: NODE.path,
      });
    });
  });

  it('collapses the File Tree without affecting the open-in-explorer action', async () => {
    // The collapse control must not steal clicks from the explorer
    // button — pin that toggling the File Tree section doesn't unmount
    // the header strip.
    render(<ProjectFilesTab />);

    await screen.findByRole('treeitem', { name: /app.ts/ });
    const treeToggle = screen.getByText('File Tree');
    fireEvent.click(treeToggle);
    fireEvent.click(treeToggle);
    await screen.findByRole('treeitem', { name: /app.ts/ });

    expect(
      screen.getByRole('button', { name: /open in file explorer/i }),
    ).toBeTruthy();
  });
});
