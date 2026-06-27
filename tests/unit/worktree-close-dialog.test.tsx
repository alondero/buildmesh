/**
 * WorktreeCloseDialog (#643)
 *
 * If the dialog is occluded (another buildmesh window on top, or the WebView
 * loses focus to a system-modal layer), the user has no way to dismiss it
 * via the backdrop click. Escape gives them a focus-independent path that
 * resolves the pending promise as 'cancel', unblocking `closingNodeIds`.
 */

import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { WorktreeCloseDialog } from '../../src/components/WorktreeCloseDialog/WorktreeCloseDialog';
import { useWorktreeClosePromptStore } from '../../src/stores/worktreeClosePromptStore';
import type { WorktreeCloseSafety } from '../../src/lib/worktreeClose';

const SAFETY: WorktreeCloseSafety = {
  worktree_path: '/repo/.claude/worktrees/occluded-node',
  has_uncommitted: true,
  has_unpushed: true,
  is_detached: false,
};

describe('WorktreeCloseDialog (#643)', () => {
  beforeEach(() => {
    useWorktreeClosePromptStore.setState({ pending: null });
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

    fireEvent.keyDown(window, { key: 'Escape' });

    await expect(actionPromise).resolves.toBe('cancel');
    expect(useWorktreeClosePromptStore.getState().pending).toBeNull();
  });

  it('does not steal Escape while no prompt is pending', () => {
    // With the useEffect gated on `pending`, no listener is installed when
    // the prompt is null — so an Escape press in the app-wide keydown
    // stream must not mutate the store.
    expect(useWorktreeClosePromptStore.getState().pending).toBeNull();

    render(<WorktreeCloseDialog />);
    fireEvent.keyDown(window, { key: 'Escape' });

    expect(useWorktreeClosePromptStore.getState().pending).toBeNull();
  });

  it('still resolves via the Cancel button (regression guard)', async () => {
    const actionPromise = useWorktreeClosePromptStore.getState().request('occluded-node', SAFETY);

    render(<WorktreeCloseDialog />);
    fireEvent.click(screen.getByRole('button', { name: /^cancel$/i }));

    await expect(actionPromise).resolves.toBe('cancel');
  });
});