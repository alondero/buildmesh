/**
 * PathHeader — shared header strip used by the Files and Changes probes.
 *
 * Pins the DOM contract the two probe tabs rely on:
 *   - path text rendered in a mono truncated span with a hover `title`
 *   - action button has role+aria-label and an SVG glyph (so a future
 *     refactor can't silently replace the icon with text/emoji)
 *   - clicking fires `open_in_file_manager` via the Tauri IPC seam with
 *     the directory as `path`
 *   - rejections are surfaced via `console.error` (not propagated) so
 *     a transient Rust-side failure doesn't crash the probe
 */

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
    // The span carries the path as its text and surfaces the full path via
    // the title attribute (so hover-tooltip reveals what `truncate` clipped).
    expect(pathSpan.getAttribute('title')).toBe('/repo/worktrees/agent-1');
    expect(pathSpan.className).toMatch(/font-mono/);
    expect(pathSpan.className).toMatch(/truncate/);
  });

  it('renders an "Open in file explorer" button containing an SVG glyph', () => {
    const { getByRole } = render(<PathHeader path="/repo" />);

    const openButton = getByRole('button', { name: /open in file explorer/i });
    // Pin that an `<svg>` lives inside the button — guards against the icon
    // being silently replaced with text/emoji in a future refactor.
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

  it('surfaces rejections via console.error instead of propagating', async () => {
    // The Rust command rejects non-existent / non-directory paths; a click
    // on a stale path must not crash the probe. Pin that we swallow and log.
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(invoke).mockRejectedValueOnce(new Error('not a directory'));

    const { getByRole } = render(<PathHeader path="/missing" />);
    fireEvent.click(getByRole('button', { name: /open in file explorer/i }));

    await waitFor(() => {
      expect(errorSpy).toHaveBeenCalled();
    });
    // No unhandled rejection — the await would have surfaced one above if so.
    errorSpy.mockRestore();
  });
});