import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ChangedFilesSection } from '../../src/components/FileTree/ChangedFilesSection';
import type { GitStatus, DiffResult } from '../../src/lib/tauri';

const FILES: GitStatus[] = [
  { path: 'src/app.ts', status: 'modified', additions: 13, deletions: 4 },
  { path: 'README.md', status: 'added', additions: 7, deletions: 0 },
];

const DIFF: DiffResult = { files: [{ path: 'src/app.ts', hunks: [] }] };

/** Route invoke() by command name so each test controls the backend response. */
function mockBackend(opts: { status?: GitStatus[] | Error; diff?: DiffResult }) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'get_git_status') {
      return opts.status instanceof Error
        ? Promise.reject(opts.status)
        : Promise.resolve(opts.status ?? []);
    }
    if (cmd === 'diff_file_against_head') {
      return Promise.resolve(opts.diff ?? DIFF);
    }
    return Promise.resolve({});
  });
}

describe('ChangedFilesSection', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('lists changed files with +additions / -deletions stats', async () => {
    mockBackend({ status: FILES });
    render(
      <ChangedFilesSection rootPath="/repo" selectedFile={null} onChangedFileSelect={() => {}} />
    );

    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('+13')).toBeTruthy();
    expect(screen.getByText('-4')).toBeTruthy();
    expect(screen.getByText('README.md')).toBeTruthy();
    expect(invoke).toHaveBeenCalledWith('get_git_status', { path: '/repo' });
  });

  it('shows "No changes" when the repo is clean', async () => {
    mockBackend({ status: [] });
    render(
      <ChangedFilesSection rootPath="/repo" selectedFile={null} onChangedFileSelect={() => {}} />
    );
    expect(await screen.findByText('No changes')).toBeTruthy();
  });

  it('loads and emits the diff when a changed file is clicked', async () => {
    const onSelect = vi.fn();
    mockBackend({ status: FILES, diff: DIFF });
    render(
      <ChangedFilesSection rootPath="/repo" selectedFile={null} onChangedFileSelect={onSelect} />
    );

    fireEvent.click(await screen.findByText('src/app.ts'));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('diff_file_against_head', {
        sessionPath: '/repo',
        filePath: 'src/app.ts',
      })
    );
    await waitFor(() => expect(onSelect).toHaveBeenCalledWith('src/app.ts', DIFF));
  });

  it('collapses the list when the header is toggled', async () => {
    mockBackend({ status: FILES });
    render(
      <ChangedFilesSection rootPath="/repo" selectedFile={null} onChangedFileSelect={() => {}} />
    );

    expect(await screen.findByText('src/app.ts')).toBeTruthy();
    fireEvent.click(screen.getByText('Changed Files'));
    expect(screen.queryByText('src/app.ts')).toBeNull();
  });
});
