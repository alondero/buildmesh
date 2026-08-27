/**
 * PrDiffView — issue #421. Renders a GitHub pull request's diff inside the
 * Center Workspace Diff Overlay (`source: 'pr'`). The data comes from
 * `GET /repos/{o}/{r}/pulls/{n}/files` via `get_pr_files`, so the diff is
 * independent of the local repo state.
 *
 * Two modes, switched by `diff.filePath`:
 *   - **List view** (`filePath === ''`): the PR's file list, each row
 *     clickable to drill in.
 *   - **File view** (`filePath !== ''`): that one file's unified `patch`
 *     text, classified +/−/context line-by-line.
 *
 * These tests pin the externally-observable contract: the fetch, the
 * toolbar breadcrumb, the list/file switch, and the patch classification.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { CenterDiffOverlay } from '../../src/components/AgentNodeView/CenterDiffOverlay';
import { useUIStore, type DiffContext } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

const MESH: Mesh = {
  id: 42,
  name: 'demo-mesh',
  path: '/repos/demo',
  layout: 'grid',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const FILES = [
  {
    filename: 'src/added.ts',
    status: 'added',
    additions: 2,
    deletions: 0,
    patch: '@@ -0,0 +1,2 @@\n+new line one\n+new line two\n',
    previous_filename: null,
  },
  {
    filename: 'src/modified.ts',
    status: 'modified',
    additions: 1,
    deletions: 1,
    patch:
      '@@ -1,2 +1,2 @@\n unchanged\n-removed line\n+added line\n',
    previous_filename: null,
  },
  {
    // GitHub's `/pulls/{n}/files` always returns the file's *current* path
    // in `filename` (the new name post-rename) and the *pre-rename* path
    // in `previous_filename` — mirrors the Rust fixture in
    // `pr_file_deserialises_rename_with_previous_filename`. The PrFileList
    // row uses `filename` as the headline and `previous_filename` as the
    // dimmed "old" path, matching what github.com shows.
    filename: 'src/new-name.ts',
    status: 'renamed',
    additions: 0,
    deletions: 0,
    patch: '',
    previous_filename: 'src/old-name.ts',
  },
];

const LIST_CTX: DiffContext = {
  filePath: '',
  rootPath: MESH.path,
  nodeId: null,
  meshId: MESH.id,
  source: 'pr',
  prNumber: 421,
};

const FILE_CTX: DiffContext = {
  ...LIST_CTX,
  filePath: 'src/modified.ts',
};

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'get_pr_files') return Promise.resolve(FILES);
    return Promise.resolve({});
  });
}

describe('PrDiffView (#421)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
    useMeshStore.setState({
      meshes: [MESH],
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    useUIStore.setState({
      probeOpen: true,
      probeTab: 'pulls',
      activeDiffFile: null,
    });
  });

  it('fetches the PR file list via get_pr_files', async () => {
    render(<CenterDiffOverlay diff={LIST_CTX} />);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_pr_files', { meshId: 42, prNumber: 421 });
    });
  });

  it('renders the PR number and mesh name in the toolbar (list view)', async () => {
    render(<CenterDiffOverlay diff={LIST_CTX} />);
    expect(await screen.findByText('PR #421')).toBeTruthy();
    expect(screen.getByText('demo-mesh')).toBeTruthy();
  });

  it('renders one row per file in the PR with status badge and +/- counts', async () => {
    render(<CenterDiffOverlay diff={LIST_CTX} />);
    // The list view shows the file paths as buttons (clickable rows).
    expect(await screen.findByRole('button', { name: /src\/added\.ts/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /src\/modified\.ts/ })).toBeTruthy();
    // The rename row shows the NEW name (the current `filename`) as the
    // prominent path and the OLD name (`previous_filename`) as the dimmed
    // "→ old-name.ts" suffix — matching what github.com shows.
    expect(screen.getByRole('button', { name: /src\/new-name\.ts/ })).toBeTruthy();
  });

  it('clicking a file in the list switches the overlay to that file\'s diff', async () => {
    render(<CenterDiffOverlay diff={LIST_CTX} />);

    // Wait for the list to render, then click the modified file.
    await screen.findByRole('button', { name: /src\/modified\.ts/ });
    fireEvent.click(screen.getByRole('button', { name: /src\/modified\.ts/ }));

    // The store should now hold a file-scoped DiffContext for the same PR.
    const after = useUIStore.getState().activeDiffFile;
    expect(after).toEqual({
      ...LIST_CTX,
      filePath: 'src/modified.ts',
    });
  });

  it('renders the file path in the toolbar (file view)', async () => {
    render(<CenterDiffOverlay diff={FILE_CTX} />);
    expect(await screen.findByText('src/modified.ts')).toBeTruthy();
    // PR #N still in the breadcrumb.
    expect(screen.getByText('PR #421')).toBeTruthy();
  });

  it('classifies patch lines into + / - / context rows (file view)', async () => {
    render(<CenterDiffOverlay diff={FILE_CTX} />);

    // The added line "added line" must be in the document (we don't pin
    // the exact <span> structure because the +/−/context backgrounds are
    // tailwind utility classes — the load-bearing assertion is the
    // classification itself, which the +/− marker conveys).
    await screen.findByText('src/modified.ts');

    // Use a regex on the marker span: the patch produces one '+' line and
    // one '-' line. They live in spans inside the body — we assert by
    // text content of the lines themselves.
    expect(screen.getByText('added line')).toBeTruthy();
    expect(screen.getByText('removed line')).toBeTruthy();
  });

  it('renders a "Binary file not shown" placeholder for files with an empty patch', async () => {
    // Use the renamed file — its patch is empty (GitHub omits it for
    // renames with no content change, and our #[serde(default)] sends ""
    // for binary files). The filePath must match the file's `filename`
    // (the new name), not `previous_filename` (the old name).
    const binaryCtx: DiffContext = { ...LIST_CTX, filePath: 'src/new-name.ts' };
    render(<CenterDiffOverlay diff={binaryCtx} />);
    expect(await screen.findByText('Binary file not shown')).toBeTruthy();
  });

  it('renders a "No files changed" empty state for a PR with zero files', async () => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_pr_files') return Promise.resolve([]);
      return Promise.resolve({});
    });
    render(<CenterDiffOverlay diff={LIST_CTX} />);
    expect(await screen.findByText('No files changed in this PR')).toBeTruthy();
  });

  it('surfaces the fetch error message verbatim', async () => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_pr_files') return Promise.reject(new Error('gh: rate limited'));
      return Promise.resolve({});
    });
    render(<CenterDiffOverlay diff={LIST_CTX} />);
    expect(await screen.findByText('gh: rate limited')).toBeTruthy();
  });

  it('surfaces a missing-prNumber programming error rather than rendering silently', async () => {
    // A caller mounting a 'pr' context without prNumber is a bug; the
    // overlay should make the error visible instead of showing "No files
    // changed" (which would be misleading).
    const broken: DiffContext = { ...LIST_CTX, prNumber: undefined };
    render(<CenterDiffOverlay diff={broken} />);
    expect(
      await screen.findByText(/PR diff opened without a prNumber/),
    ).toBeTruthy();
  });

  it('does not violate Rules-of-Hooks when prNumber flips from undefined to defined (#1242)', async () => {
    // Issue #1242: a Rules-of-Hooks crash the moment a single mounted
    // overlay re-renders with a different prNumber shape. Before the fix,
    // the `prNumber === undefined` guard short-circuited BEFORE the
    // useState/useRef/useCallback/useEffect declarations, so the first
    // render called 0 hooks and the second called 4 — React logs
    // "Rendered more hooks than during the previous render" and the
    // ErrorBoundary blanks the whole workspace.
    //
    // The fix must let the SAME fiber instance serve both shapes, with
    // every hook running unconditionally and the placeholder chosen via
    // post-hook JSX (not pre-hook early-return).
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    try {
      const broken: DiffContext = { ...LIST_CTX, prNumber: undefined };
      const { rerender } = render(<CenterDiffOverlay diff={broken} />);
      expect(
        await screen.findByText(/PR diff opened without a prNumber/),
      ).toBeTruthy();

      // The bug surfaces on this rerender — React throws the hook-order
      // error which trips the ErrorBoundary above us.
      expect(() => rerender(<CenterDiffOverlay diff={LIST_CTX} />)).not.toThrow();

      await waitFor(() => {
        expect(invoke).toHaveBeenCalledWith('get_pr_files', {
          meshId: 42,
          prNumber: 421,
        });
      });
      expect(screen.getByText('PR #421')).toBeTruthy();
      expect(
        screen.queryByText(/PR diff opened without a prNumber/),
      ).toBeNull();

      // Pin the symptom React's reconciler logs when a fiber's hook
      // count changes between renders — "more" when the new render
      // declared more hooks than the previous one, "fewer" for the
      // reverse. Our bug lands on "more"; we match both for robustness.
      const hookOrderComplaint = errSpy.mock.calls.some((args) =>
        /Rendered (more|fewer) hooks than/.test(String(args[0] ?? '')),
      );
      expect(hookOrderComplaint).toBe(false);
    } finally {
      errSpy.mockRestore();
    }
  });

  it('auto-closes when a different project is selected', async () => {
    render(<CenterDiffOverlay diff={LIST_CTX} />);
    await screen.findByText('PR #421');
    useMeshStore.setState({ selectedMeshId: 99 });
    await waitFor(() => {
      expect(useUIStore.getState().activeDiffFile).toBeNull();
    });
  });

  // Regression — issue #421. The PR source always sets `nodeId: null`
  // (the PR's head branch may not even exist locally; the auto-close is
  // mesh-scoped only). If the auto-close effect still compares
  // `activeNodeId !== diff.nodeId`, the mount-time effect fires with
  // `lensChanged = (real_id) !== null === true` and the overlay closes
  // before it paints a single frame — every time a user with a focused
  // agent node clicks "View changes". The pre-existing head/base test
  // masks this because its DiffContext binds nodeId to the focused node.
  it('survives mount when a different agent node is focused (#421 auto-close regression)', async () => {
    const focusedNode: AgentNode = {
      id: 99,
      mesh_id: MESH.id,
      name: 'some-agent',
      path: '/repos/some-agent',
      branch: 'main',
      env: 'windows',
      provider: 'anthropic',
      status: 'running',
      use_worktree: true,
      position: 0,
      created_at: '2026-01-01',
    };
    useAgentNodeStore.setState({
      agentNodes: [focusedNode],
      activeNodeId: focusedNode.id,
    });
    // Production flow: the caller (the "View changes" button) writes
    // LIST_CTX into the store; the parent (AgentNodeView) sees
    // `activeDiffFile !== null` and mounts CenterDiffOverlay. We mirror
    // that exactly so the overlay's mount-time effect runs against the
    // real production state.
    useUIStore.getState().openDiff(LIST_CTX);
    render(<CenterDiffOverlay diff={LIST_CTX} />);
    expect(await screen.findByText('PR #421')).toBeTruthy();
    // The store MUST still hold the open context — if the mount-time
    // effect fired, it would have called `closeDiff`, the parent gate
    // would have unmounted us, and `activeDiffFile` would be null.
    expect(useUIStore.getState().activeDiffFile).toEqual(LIST_CTX);
  });
});
