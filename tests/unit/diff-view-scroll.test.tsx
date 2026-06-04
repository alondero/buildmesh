import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { FileExplorerPanel } from '../../src/components/FileTree/FileExplorerPanel';
import type { FileNode, GitStatus, DiffResult, GitBranchStatus } from '../../src/lib/tauri';

const TREE: FileNode = {
  name: 'repo',
  path: '/repo',
  is_dir: true,
  children: [
    { name: 'app.ts', path: '/repo/app.ts', is_dir: false, children: [] },
  ],
};

const STATUS: GitStatus[] = [
  { path: 'app.ts', status: 'modified', additions: 2, deletions: 1 },
];

// 60 diff lines so the panel would overflow a 200px-tall container.
const DIFF: DiffResult = {
  files: [
    {
      path: 'app.ts',
      hunks: [
        {
          old_start: 1,
          old_lines: 30,
          new_start: 1,
          new_lines: 32,
          old_highlighted: '',
          new_highlighted: '',
          lines: Array.from({ length: 60 }, (_, i) => ({
            line_type: i === 10 || i === 20 ? 'add' : 'context',
            content: `line ${i}`,
            old_num: i + 1,
            new_num: i + 1,
          })),
        },
      ],
    },
  ],
};

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'list_directory') return Promise.resolve(TREE);
    if (cmd === 'get_git_status') return Promise.resolve(STATUS);
    if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
    if (cmd === 'get_git_branch_status') return Promise.resolve(null as GitBranchStatus | null);
    return Promise.resolve({});
  });
}

const noop = () => {};

describe('DiffView panel scrolling', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
  });

  it('shows the diff in a flex-column parent so the scroll container is height-constrained', async () => {
    const { container } = render(
      <FileExplorerPanel
        context={{ type: 'agent', nodeId: 1, path: '/repo' }}
        width={320}
        onWidthChange={noop}
        onClose={noop}
        nodeName="agent-1"
        changedCount={1}
      />
    );

    // Click the (only) changed file to enter the diff view. It appears
    // twice in the panel (Changed Files section + File Tree); both copies
    // wire to the same `handleChangedFileSelect` handler, so any one works.
    const fileMatches = await screen.findAllByText('app.ts');
    fireEvent.click(fileMatches[0]);

    // Wait for DiffView to render. We look for the "line N" content from the
    // first diff line, which is only present inside DiffView.
    await waitFor(() => expect(screen.getByText('line 0')).toBeTruthy());

    // The scrollable container inside DiffView is the only element with
    // class "overflow-auto" inside the panel body. (The outer panel also
    // uses overflow-hidden and overflow-auto, so we narrow to the inner one
    // by its content.)
    const scrollables = Array.from(
      container.querySelectorAll('.overflow-auto')
    ) as HTMLElement[];
    // We expect at least one scrollable: the DiffView's outer div.
    expect(scrollables.length).toBeGreaterThan(0);
    const diffScroller = scrollables[scrollables.length - 1];

    // The bug: this scroller's parent is a plain `flex-1 overflow-hidden`
    // div, NOT a flex column. That means the child's `flex-1` does not
    // participate in a flex layout, so the child grows to its content's
    // natural height and its own `overflow-auto` never engages.
    //
    // The fix: the parent must be a flex column. We assert that the
    // immediate parent has the `flex` and `flex-col` classes.
    const parent = diffScroller.parentElement!;
    expect(parent.className).toContain('flex');
    expect(parent.className).toContain('flex-col');

    // The scroller itself should keep `overflow-auto` so the user can
    // scroll the diff body when it overflows.
    expect(diffScroller.className).toContain('overflow-auto');
  });
});
