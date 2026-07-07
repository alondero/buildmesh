import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { PathHeader } from '../../src/components/shared/PathHeader';

afterEach(cleanup);

describe('PathHeader', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  it('renders the path as a mono truncated span with a hover title', () => {
    const { getByText } = render(<PathHeader path="/repo/worktrees/agent-1" />);

    const pathSpan = getByText('/repo/worktrees/agent-1');
    expect(pathSpan.getAttribute('title')).toBe('/repo/worktrees/agent-1');
    expect(pathSpan.className).toMatch(/font-mono/);
    expect(pathSpan.className).toMatch(/truncate/);
  });

  it('renders an "Open in file explorer" button containing an SVG glyph', () => {
    const { getByRole } = render(<PathHeader path="/repo" />);

    const openButton = getByRole('button', { name: /open in file explorer/i });
    // SVG-glyph pin: a future refactor that swaps the icon for text/emoji
    // would silently regress the affordance — this catches it.
    expect(openButton.querySelector('svg')).toBeTruthy();
  });

  it('fires open_in_file_manager with the path on click', async () => {
    const { getByRole } = render(<PathHeader path="/repo/worktrees/agent-1" />);

    const openButton = getByRole('button', { name: /open in file explorer/i });
    fireEvent.click(openButton);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('open_in_file_manager', {
        path: '/repo/worktrees/agent-1',
      });
    });
  });

  it('swallows IPC rejections so a stale-path click cannot crash the probe', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(invoke).mockRejectedValueOnce(new Error('not a directory'));

    const { getByRole } = render(<PathHeader path="/missing" />);
    fireEvent.click(getByRole('button', { name: /open in file explorer/i }));

    await waitFor(() => {
      expect(errorSpy).toHaveBeenCalled();
    });
    errorSpy.mockRestore();
  });
});