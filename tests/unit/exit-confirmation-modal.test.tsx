import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { ExitConfirmationModal } from '../../src/components/ExitConfirmationModal/ExitConfirmationModal';

describe('ExitConfirmationModal (issue #1501 + #1654 review round 2)', () => {
  it('renders the spec title, body count, and both actions for window-close', () => {
    render(
      <ExitConfirmationModal
        activeCount={2}
        nonResumable={[]}
        mode="window-close"
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    expect(screen.getByRole('heading', { name: 'Exit Buildmesh?' })).toBeTruthy();
    expect(screen.getByText('You have 2 active agent session(s) running.')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Keep Working' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Exit Buildmesh' })).toBeTruthy();
    expect(screen.getByTestId('exit-confirm-exit')).toBeTruthy();
  });

  // Round-2 minor 2 — pin the update-restart copy at the component
  // level. The previous test file omitted `mode` entirely and only
  // covered the window-close copy; vitest strips types so the missing
  // prop didn't fail compilation, and `tsc --noEmit` only includes
  // `src/`, so the gap was invisible to the typecheck. These tests
  // make the round-1 finding 4 fix real (a "Restart now" user must
  // see "Restart Buildmesh?" — not "Exit Buildmesh?").
  it('renders the restart title and CTA for update-restart mode', () => {
    render(
      <ExitConfirmationModal
        activeCount={2}
        nonResumable={[]}
        mode="update-restart"
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    expect(screen.getByRole('heading', { name: 'Restart Buildmesh?' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Exit Buildmesh?' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Restart now' })).toBeTruthy();
    expect(screen.getByTestId('exit-confirm-restart')).toBeTruthy();
    // The "Exiting…" wording belongs to window-close; restart uses
    // "Restarting…".
    expect(screen.queryByText('Exit Buildmesh')).toBeNull();
  });

  it('rewrites the warning text from "Exiting" to "Restarting" in update-restart mode', () => {
    render(
      <ExitConfirmationModal
        activeCount={2}
        nonResumable={[
          { id: 1, name: 'fresh-agent', providerDisplay: 'Claude Code' },
          { id: 2, name: 'shell', providerDisplay: 'Terminal' },
        ]}
        mode="update-restart"
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    const alert = screen.getByRole('alert');
    expect(alert.textContent).toContain('Restarting will permanently terminate their progress');
    expect(alert.textContent).toContain('fresh-agent (Claude Code)');
    expect(alert.textContent).toContain('shell (Terminal)');
  });

  it('warns about non-resumable sessions as Name (Provider) rows', () => {
    render(
      <ExitConfirmationModal
        activeCount={2}
        nonResumable={[
          { id: 1, name: 'fresh-agent', providerDisplay: 'Claude Code' },
          { id: 2, name: 'shell', providerDisplay: 'Terminal' },
        ]}
        mode="window-close"
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    const alert = screen.getByRole('alert');
    expect(alert.textContent).toContain('do not support resumption');
    expect(alert.textContent).toContain('Exiting will permanently terminate their progress');
    expect(alert.textContent).toContain('fresh-agent (Claude Code)');
    expect(alert.textContent).toContain('shell (Terminal)');
  });

  it('omits the warning section when every session is resumable', () => {
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        mode="window-close"
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('focuses Keep Working by default and routes both buttons', () => {
    const onKeep = vi.fn();
    const onExit = vi.fn();
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        mode="window-close"
        onKeepWorking={onKeep}
        onExit={onExit}
      />,
    );
    expect(document.activeElement?.textContent).toBe('Keep Working');
    fireEvent.click(screen.getByRole('button', { name: 'Keep Working' }));
    expect(onKeep).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { name: 'Exit Buildmesh' }));
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it('disables both actions while exiting (window-close label)', () => {
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        exiting
        mode="window-close"
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Keep Working' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('button', { name: 'Exiting…' })).toBeTruthy();
  });

  it('disables both actions while exiting (update-restart label)', () => {
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        exiting
        mode="update-restart"
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Keep Working' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('button', { name: 'Restarting…' })).toBeTruthy();
  });

  it('ignores Escape while exiting so the modal cannot vanish mid-destroy', () => {
    const onKeep = vi.fn();
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        exiting
        mode="window-close"
        onKeepWorking={onKeep}
        onExit={() => {}}
      />,
    );
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onKeep).not.toHaveBeenCalled();
  });

  it('ignores backdrop clicks while exiting', () => {
    const onKeep = vi.fn();
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        exiting
        mode="window-close"
        onKeepWorking={onKeep}
        onExit={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole('dialog').parentElement!);
    expect(onKeep).not.toHaveBeenCalled();
  });

  it('still dismisses on backdrop click when not exiting', () => {
    const onKeep = vi.fn();
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        mode="window-close"
        onKeepWorking={onKeep}
        onExit={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole('dialog').parentElement!);
    expect(onKeep).toHaveBeenCalledTimes(1);
  });
});