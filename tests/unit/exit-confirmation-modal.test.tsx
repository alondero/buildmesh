import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { ExitConfirmationModal } from '../../src/components/ExitConfirmationModal/ExitConfirmationModal';

describe('ExitConfirmationModal (issue #1501)', () => {
  it('renders the spec title, body count, and both actions', () => {
    render(
      <ExitConfirmationModal
        activeCount={2}
        nonResumable={[]}
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    expect(screen.getByRole('heading', { name: 'Exit Buildmesh?' })).toBeTruthy();
    expect(screen.getByText('You have 2 active agent session(s) running.')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Keep Working' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Exit Buildmesh' })).toBeTruthy();
  });

  it('warns about non-resumable sessions as Name (Provider) rows', () => {
    render(
      <ExitConfirmationModal
        activeCount={2}
        nonResumable={[
          { id: 1, name: 'fresh-agent', providerDisplay: 'Claude Code' },
          { id: 2, name: 'shell', providerDisplay: 'Terminal' },
        ]}
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    const alert = screen.getByRole('alert');
    expect(alert.textContent).toContain('do not support resumption');
    expect(alert.textContent).toContain('fresh-agent (Claude Code)');
    expect(alert.textContent).toContain('shell (Terminal)');
  });

  it('omits the warning section when every session is resumable', () => {
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
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

  it('disables both actions while exiting', () => {
    render(
      <ExitConfirmationModal
        activeCount={1}
        nonResumable={[]}
        exiting
        onKeepWorking={() => {}}
        onExit={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Keep Working' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('button', { name: 'Exiting…' })).toBeTruthy();
  });
});
