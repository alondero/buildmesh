import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { FileExplorerPanel } from '../../src/components/FileTree/FileExplorerPanel';
import { GIT_CHANGED } from '../../src/lib/events';
import { resetPathInvalidatedCacheForTests } from '../../src/lib/pathInvalidatedCache';
import type { DiffResult, GitBranchStatus } from '../../src/lib/tauri';

// A two-file change set with a tall first file, so the review surface must
// scroll. The agent panel fetches it all in one `diff_node_against_base` call.
const DIFF: DiffResult = {
  files: [
    {
      path: 'src/app.ts',
      status: 'modified',
      old_path: null,
      additions: 2,
      deletions: 0,
      binary: false,
      hunks: [
        {
          old_start: 1,
          old_lines: 30,
          new_start: 1,
          new_lines: 32,
          old_highlighted: '',
          new_highlighted: '',
          lines: Array.from({ length: 60 }, (_, i) => ({
            line_type: (i === 10 || i === 20 ? 'add' : 'context') as
              | 'add'
              | 'context',
            content: `line ${i}`,
            old_num: i + 1,
            new_num: i + 1,
          })),
          lines_highlighted: Array.from(
            { length: 60 },
            (_, i) => `<span class="hl">line ${i}</span>`
          ),
        },
      ],
    },
    {
      path: 'README.md',
      status: 'added',
      old_path: null,
      additions: 5,
      deletions: 0,
      binary: false,
      hunks: [
        {
          old_start: 0,
          old_lines: 0,
          new_start: 1,
          new_lines: 5,
          old_highlighted: '',
          new_highlighted: '',
          lines: [
            {
              line_type: 'add',
              content: 'hello',
              old_num: null,
              new_num: 1,
            },
          ],
          lines_highlighted: ['<span>hello</span>'],
        },
      ],
    },
  ],
};

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'diff_node_against_base') return Promise.resolve(DIFF);
    if (cmd === 'get_git_branch_status')
      return Promise.resolve(null as GitBranchStatus | null);
    return Promise.resolve({});
  });
}

const noop = () => {};

function renderPanel() {
  return render(
    <FileExplorerPanel
      context={{ type: 'agent', nodeId: 1, path: '/repo' }}
      width={360}
      onWidthChange={noop}
      onClose={noop}
      nodeName="agent-1"
      changedCount={2}
    />
  );
}

describe('Agent review surface', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
    // The path-invalidated cache primitive installs ONE process-wide
    // `GIT_CHANGED` listener for its lifetime. The global test setup
    // (vitest.setup.ts) calls `mockListeners.clear()` in its own
    // `beforeEach` to isolate per-test listeners, which removes the
    // primitive's listener too. Reset the primitive so the next mount
    // re-installs it. See issue #345.
    resetPathInvalidatedCacheForTests();
  });

  it('fetches the whole change set against the merge-base in one call', async () => {
    renderPanel();
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('diff_node_against_base', {
        nodeId: 1,
      })
    );
  });

  it('shows an aggregate summary bar', async () => {
    renderPanel();
    expect(await screen.findByText('2 files changed')).toBeTruthy();
  });

  it('stacks every file diff at once (no click-through needed)', async () => {
    renderPanel();
    // Both files render their diffs simultaneously.
    expect(await screen.findByText('line 0')).toBeTruthy();
    expect(screen.getByText('hello')).toBeTruthy();
  });

  it('renders a hunk header from the backend hunk metadata', async () => {
    renderPanel();
    expect(await screen.findByText('@@ -1,30 +1,32 @@')).toBeTruthy();
  });

  it('keeps the diff in a height-constrained, scrollable container', async () => {
    const { container } = renderPanel();
    await screen.findByText('line 0');

    const scrollers = Array.from(
      container.querySelectorAll('.overflow-auto')
    ) as HTMLElement[];
    const reviewScroller = scrollers.find(
      (el) =>
        el.className.includes('min-h-0') && el.className.includes('flex-1')
    );
    expect(reviewScroller).toBeTruthy();
  });
});

// Regression: the chip on the node title bar re-counts changes live on every
// GIT_CHANGED event, but the review panel used to snapshot the diff once on
// mount. As the agent kept editing the chip would say "1 file changed" while
// the stale panel still said "No changes vs base branch". The panel must
// re-pull on GIT_CHANGED for its own worktree path.
describe('Agent review surface — live refresh on GIT_CHANGED', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    // See the matching reset in the "Agent review surface" describe
    // block above for the rationale (setup's `mockListeners.clear()`
    // wipes the primitive's process-wide listener).
    resetPathInvalidatedCacheForTests();
  });

  it('re-fetches the diff when a GIT_CHANGED event fires for the node path', async () => {
    let diffCalls = 0;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'diff_node_against_base') {
        diffCalls += 1;
        // First read: nothing changed yet. After the watcher fires, the
        // agent's edit shows up.
        return Promise.resolve(
          (diffCalls === 1 ? { files: [] } : DIFF) as DiffResult
        );
      }
      if (cmd === 'get_git_branch_status')
        return Promise.resolve(null as GitBranchStatus | null);
      return Promise.resolve({});
    });

    renderPanel(); // context.path === '/repo'
    expect(
      await screen.findByText('No changes vs base branch')
    ).toBeTruthy();

    // The file watcher reports a change in this node's worktree.
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });

    expect(await screen.findByText('2 files changed')).toBeTruthy();
    expect(diffCalls).toBeGreaterThanOrEqual(2);
  });

  it('ignores GIT_CHANGED events for a different worktree', async () => {
    let diffCalls = 0;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'diff_node_against_base') {
        diffCalls += 1;
        return Promise.resolve({ files: [] } as DiffResult);
      }
      if (cmd === 'get_git_branch_status')
        return Promise.resolve(null as GitBranchStatus | null);
      return Promise.resolve({});
    });

    renderPanel(); // context.path === '/repo'
    await screen.findByText('No changes vs base branch');
    const callsAfterMount = diffCalls;

    await act(async () => {
      await emit(GIT_CHANGED, { path: '/some/other/worktree' });
    });

    expect(diffCalls).toBe(callsAfterMount);
  });
});
