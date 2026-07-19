/**
 * UserConfigPanel — issue #60. Right-dock panel that lists the resolved
 * ~/.claude tree for browsing, with files opening in the user's default
 * editor instead of an inline diff.
 *
 * Why it is NOT a Probe tab
 * -------------------------
 * The Probe Panel anchors on `useProbeContext()` (mesh-scoped); the
 * `useUIStore` already gates the `usage` tab as the lone host-scoped tab
 * (no mesh required), but that tab is a glanceable surface, not a tree
 * browser. User Config deserves its own panel because it owns its
 * visibility (`userConfigOpen`), its resolved path is fetched exactly
 * once on mount, and clicking through to an external editor doesn't fit
 * the Probe's review/diff flow.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { UserConfigPanel } from '../../src/components/Sidebar/UserConfigPanel';
import { useUIStore } from '../../src/stores/uiStore';
import type { FileNode } from '../../src/lib/tauri';

const FAKE_CLAUDE_DIR = '/home/user/.claude';

const TREE: FileNode = {
  name: '.claude',
  path: FAKE_CLAUDE_DIR,
  is_dir: true,
  children: [
    {
      name: 'CLAUDE.md',
      path: `${FAKE_CLAUDE_DIR}/CLAUDE.md`,
      is_dir: false,
      children: [],
    },
    {
      name: 'projects',
      path: `${FAKE_CLAUDE_DIR}/projects`,
      is_dir: true,
      children: [
        {
          name: 'a.jsonl',
          path: `${FAKE_CLAUDE_DIR}/projects/a.jsonl`,
          is_dir: false,
          children: [],
        },
      ],
    },
  ],
};

/** Wire the backend mock exactly the way the component calls it. The
 *  FileTree's `useChangedFiles` hook short-circuits on `showGitStatus=false`,
 *  so we only need to mock `get_user_config_dir` and `list_directory` —
 *  no `get_git_status` should ever fire on this surface. */
function mockBackend(opts: { configDir?: string; tree?: FileNode } = {}) {
  const configDir = opts.configDir ?? FAKE_CLAUDE_DIR;
  const tree = opts.tree ?? TREE;
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'get_user_config_dir') return Promise.resolve(configDir);
    if (cmd === 'list_directory') {
      expect(args?.maxDepth).toBe(4); // issue #376 default
      return Promise.resolve(tree);
    }
    if (cmd === 'open_in_editor') return Promise.resolve(undefined);
    return Promise.resolve({});
  });
}

describe('UserConfigPanel (#60)', () => {
  beforeEach(() => {
    useUIStore.setState({ userConfigOpen: true });
  });

  afterEach(() => {
    cleanup();
    useUIStore.setState({ userConfigOpen: false });
  });

  it('resolves ~/.claude on mount and renders the tree at that path', async () => {
    mockBackend();
    render(<UserConfigPanel />);

    // Header text is "User Config" per the issue acceptance criterion
    // (panel header shows "User Config").
    expect(screen.getByText('User Config')).toBeTruthy();
    // FileTree renders FileNodes — the two top-level children of FAKE_CLAUDE_DIR.
    expect(await screen.findByText('CLAUDE.md')).toBeTruthy();
    expect(screen.getByText('projects')).toBeTruthy();
  });

  it('shows no M badges (no git status fetch fires)', async () => {
    mockBackend();
    render(<UserConfigPanel />);

    await screen.findByText('CLAUDE.md');
    await waitFor(() => {
      const calls = vi.mocked(invoke).mock.calls.map(([c]) => c);
      expect(calls).not.toContain('get_git_status');
    });
  });

  it('routes a file click to open_in_editor with the absolute path', async () => {
    mockBackend();
    render(<UserConfigPanel />);

    await screen.findByText('CLAUDE.md');
    await userEvent.click(screen.getByText('CLAUDE.md'));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'open_in_editor',
        expect.objectContaining({ path: `${FAKE_CLAUDE_DIR}/CLAUDE.md` }),
      );
    });
  });

  it('renders a close button that clears userConfigOpen', async () => {
    mockBackend();
    render(<UserConfigPanel />);

    // Accessible name on the close button is "Close user config panel".
    await userEvent.click(screen.getByRole('button', { name: 'Close user config panel' }));
    expect(useUIStore.getState().userConfigOpen).toBe(false);
  });

  it('renders a friendly empty state when the resolved path is missing', async () => {
    mockBackend({ configDir: '/nope/__does_not_exist__', tree: TREE });
    // Force list_directory to fail the way the backend does when the dir is gone.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_user_config_dir') return Promise.resolve('/nope/__does_not_exist__');
      if (cmd === 'list_directory') {
        return Promise.reject('Path does not exist: /nope/__does_not_exist__');
      }
      return Promise.resolve({});
    });
    render(<UserConfigPanel />);

    // FileTree surfaces the formatError string; we assert on the FAIL prefix
    // because the exact wording may grow over time.
    await waitFor(() => {
      const matches = screen.getAllByText(/does not exist/i);
      expect(matches.length).toBeGreaterThan(0);
    });
  });
});
