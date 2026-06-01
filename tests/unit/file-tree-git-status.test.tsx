import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
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
});
