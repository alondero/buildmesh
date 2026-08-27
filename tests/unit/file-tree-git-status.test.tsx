import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { FileTree } from '../../src/components/FileTree/FileTree';
import type { FileNode, GitStatus, DiffResult } from '../../src/lib/tauri';

// Issue #1245 — the diff-load failure path now surfaces the error via
// the shared `addToast` wrapper AND rolls back the optimistic highlight
// so the row doesn't keep claiming an open diff with nothing actually
// loaded. Mock the wrapper with the same `vi.hoisted` + named-export
// pattern used by `agent-node-store.test.ts` and
// `git-issues-tab.test.tsx` — production code imports the named
// export, tests capture the spy via `vi.mocked(addToast)`.
const { addToastMock } = vi.hoisted(() => ({
  addToastMock: vi.fn(),
}));
vi.mock('../../src/stores/toastStore', () => ({
  addToast: addToastMock,
  // `dismissToast` is exported by the module but isn't exercised here;
  // pass through to keep the module shape intact.
  dismissToast: vi.fn(),
}));
import { addToast } from '../../src/stores/toastStore';

const mockAddToast = vi.mocked(addToast);

// list_directory returns ABSOLUTE node paths; get_git_status returns paths
// RELATIVE to the repo root. The tree must reconcile the two so changed files
// are badged and their diffs load.
const TREE: FileNode = {
  name: 'repo',
  path: '/repo',
  is_dir: true,
  children: [
    {
      name: 'src',
      path: '/repo/src',
      is_dir: true,
      children: [
        { name: 'app.ts', path: '/repo/src/app.ts', is_dir: false, children: [] },
      ],
    },
    { name: 'README.md', path: '/repo/README.md', is_dir: false, children: [] },
  ],
};

const STATUS: GitStatus[] = [
  { path: 'src/app.ts', status: 'modified', additions: 2, deletions: 1 },
];

const DIFF: DiffResult = { files: [{ path: 'src/app.ts', hunks: [] }] };

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'list_directory') return Promise.resolve(TREE);
    if (cmd === 'get_git_status') return Promise.resolve(STATUS);
    if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
    return Promise.resolve({});
  });
}

const noop = () => {};

describe('FileTree git status reconciliation', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
    mockAddToast.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('badges a changed file even though git status paths are repo-relative', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={true}
        onChangedFileSelect={noop}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );

    // Expand src/ so the changed file row is visible.
    fireEvent.click(await screen.findByText('src'));
    const row = (await screen.findByText('app.ts')).closest('div')!;
    // The 'M' status badge should appear on the changed file's row.
    expect(row.textContent).toContain('M');
  });

  it('loads the diff using the repo-relative path, not the absolute node path', async () => {
    const onChangedFileSelect = vi.fn();
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={true}
        onChangedFileSelect={onChangedFileSelect}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );

    fireEvent.click(await screen.findByText('src'));
    fireEvent.click(await screen.findByText('app.ts'));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('diff_file_against_head', {
        sessionPath: '/repo',
        filePath: 'src/app.ts',
      })
    );
    await waitFor(() => expect(onChangedFileSelect).toHaveBeenCalled());
  });

  // Regression for issue #804: a background GIT_CHANGED refresh used to
  // blank the whole tree behind a full-panel spinner, unmounting every
  // `TreeNode` and losing its local `expanded` state — every folder the
  // user had opened snapped shut on each git-status poll.
  it('keeps an expanded folder open across a background GIT_CHANGED refresh', async () => {
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={true}
        onChangedFileSelect={noop}
        onUnchangedFileSelect={noop}
        selectedFile={null}
        onFileSelect={noop}
      />
    );

    fireEvent.click(await screen.findByText('src'));
    expect(await screen.findByTestId('folder-expanded')).toBeTruthy();

    // A second, distinct git status so the refreshed badge is observable.
    const UPDATED_STATUS: GitStatus[] = [
      { path: 'src/app.ts', status: 'modified', additions: 3, deletions: 1 },
      { path: 'README.md', status: 'modified', additions: 1, deletions: 0 },
    ];
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'list_directory') return Promise.resolve(TREE);
      if (cmd === 'get_git_status') return Promise.resolve(UPDATED_STATUS);
      if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
      return Promise.resolve({});
    });

    // Jump past the shared gitStatusClient's freshness window
    // (minRefetchIntervalMs: 2_000) so the GIT_CHANGED event triggers an
    // immediate refetch instead of a deferred trailing one.
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 10_000);
    await act(async () => {
      await emit('git-changed', { path: '/repo' });
    });

    // The badge on the newly-changed README picks up the refresh...
    const readmeRow = (await screen.findByText('README.md')).closest('div')!;
    await waitFor(() => expect(readmeRow.textContent).toContain('M'));
    // ...and the folder the user expanded is still open, with its child
    // still visible — the tree was never unmounted underneath it.
    expect(screen.getByTestId('folder-expanded')).toBeTruthy();
    expect(screen.getByText('app.ts')).toBeTruthy();
  });

  // Issue #1245 — clicking a changed file used to set the highlight
  // optimistically, then swallow the diff-load failure into
  // `console.error`. The row sat selected as if a diff had opened while
  // nothing actually did. The fix surfaces the error through `addToast`
  // AND rolls the optimistic `onFileSelect` back so the highlight
  // disappears. The rollback target is whatever `selectedFile` was
  // *before* the click — a bare `onFileSelect(null)` would clobber any
  // pre-existing selection, so we restore the previous value.
  it('rolls back the optimistic selection when diff_file_against_head rejects', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'list_directory') return Promise.resolve(TREE);
      if (cmd === 'get_git_status') return Promise.resolve(STATUS);
      // Reject the diff load — the realistic case (worktree lock,
      // oversized binary, transient git error).
      if (cmd === 'diff_file_against_head') return Promise.reject(new Error('worktree lock'));
      return Promise.resolve({});
    });

    const onChangedFileSelect = vi.fn();
    const onFileSelect = vi.fn();
    render(
      <FileTree
        rootPath="/repo"
        showGitStatus={true}
        onChangedFileSelect={onChangedFileSelect}
        onUnchangedFileSelect={noop}
        // Pre-existing selection: user already had README open. A bare
        // `onFileSelect(null)` rollback would clobber it too — the
        // implementation captures `selectedFile` and restores it.
        selectedFile="/repo/README.md"
        onFileSelect={onFileSelect}
      />
    );

    fireEvent.click(await screen.findByText('src'));
    fireEvent.click(await screen.findByText('app.ts'));

    // Diff load was attempted with the repo-relative path.
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('diff_file_against_head', {
        sessionPath: '/repo',
        filePath: 'src/app.ts',
      }),
    );
    // Toast surfaces the failure (provider + message + severity match
    // the sibling ChangedFilesSection call site so identical failures
    // dedup under one slot).
    await waitFor(() =>
      expect(mockAddToast).toHaveBeenCalledWith(
        'Review',
        expect.stringContaining('Failed to load diff for src/app.ts'),
        'error',
      ),
    );
    // `formatError` unwraps `e.message` — the toast body carries the
    // raw backend string, not the bogus 'Error: ' prefix.
    await waitFor(() =>
      expect(mockAddToast).toHaveBeenCalledWith(
        'Review',
        expect.stringContaining('worktree lock'),
        'error',
      ),
    );
    // The callback that would have shown the diff in the Review pane
    // was NEVER invoked — nothing to forward.
    expect(onChangedFileSelect).not.toHaveBeenCalled();
    // The optimistic `onFileSelect(app.ts)` fires first, then the
    // rollback restores the previous selection. The capture is read at
    // click time so the rollback target is `/repo/README.md`, not null.
    await waitFor(() =>
      expect(onFileSelect).toHaveBeenNthCalledWith(1, '/repo/src/app.ts'),
    );
    // The second `onFileSelect` call is the rollback. Critically, it
    // restores the prior selection rather than passing null — that
    // distinction is what makes the highlight stop lying without also
    // stomping a different row the user already had open.
    await waitFor(() =>
      expect(onFileSelect).toHaveBeenNthCalledWith(2, '/repo/README.md'),
    );
    expect(onFileSelect).not.toHaveBeenCalledWith(null);
  });
});
