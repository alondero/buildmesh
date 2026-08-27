import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ChangedFilesSection } from '../../src/components/FileTree/ChangedFilesSection';
import type { GitStatus, DiffResult } from '../../src/lib/tauri';

// Issue #1245 — the click handler now surfaces diff-load failures through
// the shared `addToast` wrapper from `stores/toastStore` instead of
// swallowing them. Mock the wrapper with the same `vi.hoisted` + named-
// export pattern used by `agent-node-store.test.ts` and
// `git-issues-tab.test.tsx` so the production module imports resolve to
// the spy. `mockReset()` in `beforeEach` keeps each test independent.
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

const FILES: GitStatus[] = [
  { path: 'src/app.ts', status: 'modified', additions: 13, deletions: 4 },
  { path: 'README.md', status: 'added', additions: 7, deletions: 0 },
];

const DIFF: DiffResult = { files: [{ path: 'src/app.ts', hunks: [] }] };

/** Route invoke() by command name so each test controls the backend response. */
function mockBackend(opts: { status?: GitStatus[] | Error; diff?: DiffResult | Error }) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'get_git_status') {
      return opts.status instanceof Error
        ? Promise.reject(opts.status)
        : Promise.resolve(opts.status ?? []);
    }
    if (cmd === 'diff_file_against_head') {
      return opts.diff instanceof Error
        ? Promise.reject(opts.diff)
        : Promise.resolve(opts.diff ?? DIFF);
    }
    return Promise.resolve({});
  });
}

describe('ChangedFilesSection', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockAddToast.mockReset();
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

  // Issue #1245 — clicking a changed file used to swallow
  // `diff_file_against_head` rejections into `console.error`, leaving the
  // row highlight in place (well, `selectedFile` is parent-owned here so
  // no local highlight changes — but onChangedFileSelect still wouldn't
  // fire, so the Review pane stays empty with no toast). Now the failure
  // surfaces via the shared `addToast` wrapper and the parent callback
  // is NOT invoked (nothing to forward to, so the Review pane stays
  // empty as before — but the user gets a visible reason).
  it('surfaces diff-load failure through addToast and does not invoke the callback', async () => {
    const onSelect = vi.fn();
    mockBackend({ status: FILES, diff: new Error('backend lock') });
    render(
      <ChangedFilesSection rootPath="/repo" selectedFile={null} onChangedFileSelect={onSelect} />
    );

    fireEvent.click(await screen.findByText('src/app.ts'));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('diff_file_against_head', {
        sessionPath: '/repo',
        filePath: 'src/app.ts',
      }),
    );
    await waitFor(() =>
      expect(mockAddToast).toHaveBeenCalledWith(
        'Review',
        expect.stringContaining('Failed to load diff for src/app.ts'),
        'error',
      ),
    );
    // `formatError` strips the bogus 'Error: ' prefix that `String(e)`
    // adds — assert the unwrapped message reaches the toast body.
    await waitFor(() =>
      expect(mockAddToast).toHaveBeenCalledWith(
        'Review',
        expect.stringContaining('backend lock'),
        'error',
      ),
    );
    expect(onSelect).not.toHaveBeenCalled();
  });
});
