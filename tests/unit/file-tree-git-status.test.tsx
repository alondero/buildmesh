import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { FileTree } from '../../src/components/FileTree/FileTree';
import type { FileNode, GitStatus, DiffResult } from '../../src/lib/tauri';

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
});
