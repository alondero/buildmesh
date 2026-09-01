/**
 * DiffOverlayShell + Diff view toggle — issue #1374.
 *
 * The shell owns the Unified/Split toggle (persisted in localStorage under
 * `buildmesh.diff-view`), and the head/base body threads the mode into
 * `<Diff>` via the shell's render-prop. Quick actions (Stage File / Revert
 * File / Copy Diff) appear only for head/base diffs.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { CenterDiffOverlay } from '../../src/components/AgentNodeView/CenterDiffOverlay';
import { DIFF_VIEW_STORAGE_KEY, loadDiffViewMode } from '../../src/components/Diff/Diff';
import { useUIStore, type DiffContext } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
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

const DIFF = {
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

// Sample bulk file list returned by `node_changed_files` for the
// Quick File Navigation Drawer (issue #1374, item 3). Three files
// covering all three badge kinds so the test can assert each one
// is rendered with its expected M/A/D label + +/- counts.
const CHANGED_FILES = [
  { path: 'src/app.ts', status: 'modified', additions: 3, deletions: 2 },
  { path: 'src/new.ts', status: 'added', additions: 5, deletions: 0 },
  { path: 'src/gone.ts', status: 'deleted', additions: 0, deletions: 4 },
];

const BASE_CTX: DiffContext = {
  filePath: 'src/app.ts',
  rootPath: '/repo/worktrees/fix-the-bug',
  nodeId: 7,
  meshId: 1,
  source: 'base',
};

describe('Diff view toggle (issue #1374)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'diff_node_file_against_base') return Promise.resolve(DIFF);
      if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
      if (cmd === 'node_changed_files') return Promise.resolve(CHANGED_FILES);
      if (cmd === 'get_git_status') return Promise.resolve(CHANGED_FILES);
      return Promise.resolve({});
    });
    useMeshStore.setState({
      meshes: [MESH],
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    seedAgentNodes([NODE], NODE.id);
    useUIStore.setState({ probeOpen: true, probeTab: 'files', activeDiffFile: BASE_CTX });
    localStorage.clear();
    // jsdom doesn't define `navigator.clipboard` — the overlay's Copy
    // Diff button calls `navigator.clipboard.writeText` synchronously,
    // so without this stub the test crashes on click instead of failing
    // the assertion cleanly. (userEvent.setup() would also work, but we
    // use fireEvent for determinism — see #1374 review note on
    // deterministic vs simulated input.) The stub persists across tests
    // within this file but is fine for the surrounding suite because
    // navigator.clipboard is read-only on the real navigator and any
    // other test that needs a writeText can re-spy on the now-defined
    // object.
    if (!navigator.clipboard) {
      Object.defineProperty(navigator, 'clipboard', {
        configurable: true,
        value: { writeText: vi.fn().mockResolvedValue(undefined) },
      });
    } else {
      vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue(undefined);
    }
  });

  // Reset the shared UI store + mesh + agent node store after each test
  // so a later test file's test isn't surprised by `activeDiffFile`
  // pointing at our node, the seed mesh being selected, or NODE.id
  // lingering in `nodesById`/`closingNodeIds`. These are module-level
  // singletons; without this reset the diff overlay's `activeDiffFile`
  // set in `beforeEach` (and any `closingNodeIds` flip from a
  // `stageFile` / `revertFile` failure) leaks into any subsequent test
  // that gates render on those values (a Rules-of-Hooks landmine —
  // conditional rendering after a store-driven hook count change trips
  // React's "Rendered fewer hooks than expected" assertion).
  afterEach(() => {
    useUIStore.setState({
      probeOpen: false,
      probeTab: null,
      activeDiffFile: null,
    });
    useMeshStore.setState({
      meshes: [],
      meshesById: new Map(),
      selectedMeshId: null,
    });
    useAgentNodeStore.setState({
      nodesById: {},
      nodeIds: [],
      activeNodeId: null,
      closingNodeIds: new Set(),
      autopilotStates: {},
      semanticTurns: {},
      schedules: {},
    });
  });

  it('defaults to unified and persists split preference in localStorage', async () => {
    expect(loadDiffViewMode()).toBe('unified');
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    const toggle = await screen.findByTestId('diff-view-toggle');
    expect(toggle.textContent).toContain('Unified');

    fireEvent.click(toggle);
    expect(toggle.textContent).toContain('Split');
    expect(localStorage.getItem(DIFF_VIEW_STORAGE_KEY)).toBe('split');
    expect(loadDiffViewMode()).toBe('split');
  });

  it('boots in split mode when the preference is persisted', async () => {
    localStorage.setItem(DIFF_VIEW_STORAGE_KEY, 'split');
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    const toggle = await screen.findByTestId('diff-view-toggle');
    expect(toggle.textContent).toContain('Split');
  });

  it('renders split rows with aligned old/new cells for a remove+add pair', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByText('src/app.ts');
    // Split mode: the paired remove+add renders "old" on the left and
    // "new" on the right in the same row.
    fireEvent.click(screen.getByTestId('diff-view-toggle'));
    await waitFor(() => {
      expect(screen.getByText('old')).toBeTruthy();
      expect(screen.getByText('new')).toBeTruthy();
    });
    // Both cells of the aligned pair exist in the same split row.
    const rows = screen.getByText('old').closest('.flex')?.parentElement;
    expect(rows?.textContent).toContain('old');
    expect(rows?.textContent).toContain('new');
  });

  it('shows Stage File / Revert File / Copy Diff quick actions for head/base diffs', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    expect(await screen.findByTestId('diff-quick-actions')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Stage File' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Revert File' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Copy Diff' })).toBeTruthy();
  });

  it('stage_file command stages a tracked file via the backend', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByTestId('diff-quick-actions');
    fireEvent.click(screen.getByRole('button', { name: 'Stage File' }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('stage_file', {
        repoPath: '/repo/worktrees/fix-the-bug',
        filePath: 'src/app.ts',
      });
    });
  });

  it('revert_file command reverts a tracked file via the backend (after confirm)', async () => {
    // Issue #1374 review: Revert is destructive (discards uncommitted
    // work or deletes untracked files), so the button now opens a
    // ConfirmDialog first. The actual `revert_file` IPC fires only
    // after the user confirms.
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByTestId('diff-quick-actions');
    fireEvent.click(screen.getByRole('button', { name: 'Revert File' }));
    // ConfirmDialog appears; the destructive label is "Revert".
    await screen.findByRole('button', { name: 'Revert', hidden: false });
    fireEvent.click(screen.getByRole('button', { name: 'Revert', hidden: false }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('revert_file', {
        repoPath: '/repo/worktrees/fix-the-bug',
        filePath: 'src/app.ts',
      });
    });
  });

  it('revert_file cancel keeps the file untouched', async () => {
    // The Cancel button on the destructive confirm MUST NOT fire the
    // backend — that's the entire point of the gate. Test pins the
    // invariant.
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByTestId('diff-quick-actions');
    fireEvent.click(screen.getByRole('button', { name: 'Revert File' }));
    await screen.findByRole('button', { name: 'Cancel', hidden: false });
    fireEvent.click(screen.getByRole('button', { name: 'Cancel', hidden: false }));
    // Give any stray fire a chance to settle — Cancel must suppress the IPC.
    await new Promise((r) => setTimeout(r, 50));
    expect(invoke).not.toHaveBeenCalledWith('revert_file', expect.anything());
  });

  it('stage_file failure surfaces as a toast (not silent console.error)', async () => {
    // Issue #1374 review: the previous shape swallowed errors into a
    // console.error — destructive failures left the user with no UI
    // signal. Pin that stage failures now flow through `addToast`.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'stage_file') return Promise.reject('boom: index locked');
      if (cmd === 'diff_node_file_against_base') return Promise.resolve(DIFF);
      if (cmd === 'node_changed_files') return Promise.resolve(CHANGED_FILES);
      return Promise.resolve({});
    });
    const toastSpy = vi.fn();
    useToastStore.setState({ toasts: [], addToast: toastSpy });
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByTestId('diff-quick-actions');
    fireEvent.click(screen.getByRole('button', { name: 'Stage File' }));
    await waitFor(() => {
      expect(toastSpy).toHaveBeenCalledWith(
        'DiffOverlay',
        expect.stringContaining('Stage failed'),
        'error',
      );
    });
  });

  it('copy_diff writes the unified diff text to the clipboard', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByTestId('diff-quick-actions');
    fireEvent.click(screen.getByRole('button', { name: 'Copy Diff' }));
    await waitFor(() => {
      expect(navigator.clipboard.writeText).toHaveBeenCalled();
    });
  });

  // ─── Quick File Navigation Drawer (issue #1374, item 3) ──────────────

  it('drawer is hidden by default (closed-collapsible)', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    await screen.findByTestId('diff-file-nav-toggle');
    expect(screen.queryByTestId('diff-file-nav')).toBeNull();
  });

  it('clicking the toggle opens the drawer and renders file rows', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    fireEvent.click(await screen.findByTestId('diff-file-nav-toggle'));

    // Drawer fetches `node_changed_files` for a base diff lens.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('node_changed_files', { nodeId: 7 });
    });

    // Each row renders with its M/A/D badge + +/- counts.
    const rows = await screen.findAllByTestId('diff-file-nav-row');
    expect(rows).toHaveLength(3);
    expect(rows[0].getAttribute('data-path')).toBe('src/app.ts');
    expect(rows[0].textContent).toContain('M');
    expect(rows[0].textContent).toContain('+3');
    expect(rows[0].textContent).toContain('-2');
    expect(rows[1].textContent).toContain('A');
    expect(rows[1].textContent).toContain('+5');
    expect(rows[2].textContent).toContain('D');
    expect(rows[2].textContent).toContain('-4');
  });

  it('marks the currently-viewed file as the active row', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    fireEvent.click(await screen.findByTestId('diff-file-nav-toggle'));
    const rows = await screen.findAllByTestId('diff-file-nav-row');
    const currentRow = rows.find(
      (r) => r.getAttribute('data-path') === BASE_CTX.filePath,
    );
    expect(currentRow?.getAttribute('aria-current')).toBe('true');
  });

  it('clicking a row jumps to that file via openDiff', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    fireEvent.click(await screen.findByTestId('diff-file-nav-toggle'));
    const rows = await screen.findAllByTestId('diff-file-nav-row');
    // Click the "added" row.
    fireEvent.click(rows[1]);

    await waitFor(() => {
      const ctx = useUIStore.getState().activeDiffFile;
      expect(ctx).not.toBeNull();
      expect(ctx?.filePath).toBe('src/new.ts');
      expect(ctx?.source).toBe('base');
      expect(ctx?.nodeId).toBe(7);
    });
  });

  it('clicking the toggle a second time hides the drawer', async () => {
    render(<CenterDiffOverlay diff={BASE_CTX} />);
    const toggle = await screen.findByTestId('diff-file-nav-toggle');
    fireEvent.click(toggle);
    expect(screen.queryByTestId('diff-file-nav')).not.toBeNull();
    fireEvent.click(toggle);
    expect(screen.queryByTestId('diff-file-nav')).toBeNull();
  });

  it('head diffs fetch the bulk list via get_git_status, not node_changed_files', async () => {
    useUIStore.setState({
      probeOpen: true,
      probeTab: 'files',
      activeDiffFile: { ...BASE_CTX, source: 'head', nodeId: null },
    });
    render(<CenterDiffOverlay diff={{ ...BASE_CTX, source: 'head', nodeId: null }} />);
    fireEvent.click(await screen.findByTestId('diff-file-nav-toggle'));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_git_status', {
        path: BASE_CTX.rootPath,
      });
    });
  });
});
