/**
 * WorktreeCloseDialog (#643)
 *
 * If the dialog is occluded (another buildmesh window on top, or the WebView
 * loses focus to a system-modal layer), the user has no way to dismiss it
 * via the backdrop click. Escape gives them a focus-independent path that
 * resolves the pending promise as 'cancel', unblocking `closingNodeIds`.
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { WorktreeCloseDialog } from '../../src/components/WorktreeCloseDialog/WorktreeCloseDialog';
import { useWorktreeClosePromptStore } from '../../src/stores/worktreeClosePromptStore';
import type { WorktreeCloseSafety } from '../../src/lib/worktreeClose';
import type { GitStatus } from '../../src/types/generated/GitStatus';

// The dialog reads the uncommitted file list through the shared git-status
// hook; stub it so these tests don't reach for a Tauri backend and can drive
// the rendered list deterministically.
const changed = vi.hoisted(() => ({ files: [] as GitStatus[], loading: false }));
vi.mock('../../src/hooks/useChangedFiles', () => ({
  useChangedFiles: () => ({ files: changed.files, loading: changed.loading }),
}));

const SAFETY: WorktreeCloseSafety = {
  worktree_path: '/repo/.claude/worktrees/occluded-node',
  has_uncommitted: true,
  has_unpushed: true,
  is_detached: false,
};

describe('WorktreeCloseDialog (#643)', () => {
  beforeEach(() => {
    useWorktreeClosePromptStore.setState({ pending: null });
    changed.files = [];
    changed.loading = false;
  });

  it('renders nothing when no prompt is pending', () => {
    const { container } = render(<WorktreeCloseDialog />);
    expect(container.firstChild).toBeNull();
  });

  it('renders the header, risk copy, and three choices when a prompt is pending', async () => {
    const actionPromise = useWorktreeClosePromptStore.getState().request('occluded-node', SAFETY);

    render(<WorktreeCloseDialog />);

    expect(screen.getByRole('heading', { name: /Remove agent worktree/i })).toBeTruthy();
    expect(screen.getByText(/has uncommitted changes and unpushed or unmerged commits/)).toBeTruthy();
    expect(screen.getByRole('button', { name: /^cancel$/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /keep worktree/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /remove anyway/i })).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: /^cancel$/i }));
    await actionPromise;
  });

  it('resolves the prompt as "cancel" when Escape is pressed (#643)', async () => {
    const actionPromise = useWorktreeClosePromptStore.getState().request('occluded-node', SAFETY);

    render(<WorktreeCloseDialog />);
    // Sanity-check the dialog is up before we send the key.
    expect(screen.getByRole('heading', { name: /Remove agent worktree/i })).toBeTruthy();

    fireEvent.keyDown(document, { key: 'Escape' });

    await expect(actionPromise).resolves.toBe('cancel');
    expect(useWorktreeClosePromptStore.getState().pending).toBeNull();
  });

  it('does not steal Escape while no prompt is pending', () => {
    // With the useEffect gated on `pending`, no listener is installed when
    // the prompt is null — so an Escape press in the app-wide keydown
    // stream must not mutate the store.
    expect(useWorktreeClosePromptStore.getState().pending).toBeNull();

    render(<WorktreeCloseDialog />);
    fireEvent.keyDown(document, { key: 'Escape' });

    expect(useWorktreeClosePromptStore.getState().pending).toBeNull();
  });

  it('still resolves via the Cancel button (regression guard)', async () => {
    const actionPromise = useWorktreeClosePromptStore.getState().request('occluded-node', SAFETY);

    render(<WorktreeCloseDialog />);
    fireEvent.click(screen.getByRole('button', { name: /^cancel$/i }));

    await expect(actionPromise).resolves.toBe('cancel');
  });

  it('lists the uncommitted files so the user can see what would be discarded', () => {
    changed.files = [
      { path: 'src/app.ts', status: 'modified', additions: 3, deletions: 1 },
      { path: 'README.md', status: 'added', additions: 10, deletions: 0 },
    ];
    useWorktreeClosePromptStore.getState().request('occluded-node', SAFETY);

    render(<WorktreeCloseDialog />);

    expect(screen.getByText('src/app.ts')).toBeTruthy();
    expect(screen.getByText('README.md')).toBeTruthy();
    // Per-file line counts help judge the size of what's being discarded.
    expect(screen.getByText('+3')).toBeTruthy();
    expect(screen.getByText('-1')).toBeTruthy();
  });

  it('shows a loading placeholder while the uncommitted list is fetching', () => {
    changed.files = [];
    changed.loading = true;
    useWorktreeClosePromptStore.getState().request('occluded-node', SAFETY);

    render(<WorktreeCloseDialog />);

    expect(screen.getByText(/loading/i)).toBeTruthy();
  });

  it('does not falsely reassure when the file list could not be loaded', () => {
    // The backend already flagged has_uncommitted, so an empty, non-loading
    // list means get_git_status failed (the shared hook collapses errors to
    // []). "No uncommitted files" would wrongly imply nothing is at risk right
    // before the user clicks "Remove anyway".
    changed.files = [];
    changed.loading = false;
    useWorktreeClosePromptStore.getState().request('occluded-node', SAFETY);

    render(<WorktreeCloseDialog />);

    expect(screen.queryByText(/no uncommitted files/i)).toBeNull();
    expect(screen.getByText(/couldn.t list the changed files/i)).toBeTruthy();
  });

  it('omits the file list when there are no uncommitted changes (unpushed-only)', () => {
    changed.files = [];
    useWorktreeClosePromptStore.getState().request('unpushed-node', {
      ...SAFETY,
      has_uncommitted: false,
    });

    render(<WorktreeCloseDialog />);

    // Only the unpushed-commits risk remains; no "uncommitted files" section.
    expect(screen.getByText(/has unpushed or unmerged commits/)).toBeTruthy();
    expect(screen.queryByText(/uncommitted files/i)).toBeNull();
  });
});