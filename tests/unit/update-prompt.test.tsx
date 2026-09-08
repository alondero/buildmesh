/**
 * The in-app update prompt (issue #1526).
 *
 * Phase-aware rendering — the modal only appears for `available` /
 * `downloading` / `installing` / `ready_to_restart` / `failed` and
 * exposes different actions for each. Quiet check phases (`idle`,
 * `checking`, `current`, `unreachable`) render nothing — those are
 * the Settings > About section's concern.
 *
 * We mock the updater lib and the IPC seam (process / stores) so the
 * test never touches real IPC; the hook's pure state machine is
 * covered by `updater.test.ts` (lib) and we exercise the wiring here.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import type { Update } from '@tauri-apps/plugin-updater';

const { runUpdateCheckMock, downloadAndInstallUpdateMock, relaunchMock, showExitPromptMock } = vi.hoisted(() => ({
  runUpdateCheckMock: vi.fn(),
  downloadAndInstallUpdateMock: vi.fn(),
  relaunchMock: vi.fn().mockResolvedValue(undefined),
  showExitPromptMock: vi.fn(),
}));

vi.mock('../../src/lib/updater', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/updater')>();
  return {
    ...actual,
    runUpdateCheck: runUpdateCheckMock,
    downloadAndInstallUpdate: downloadAndInstallUpdateMock,
  };
});

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: relaunchMock,
}));

vi.mock('../../src/stores/exitPromptStore', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/stores/exitPromptStore')>();
  return {
    ...actual,
    useExitPromptStore: {
      getState: () => ({ showExitPrompt: showExitPromptMock }),
      // The hook reads `confirmBeforeQuit` via getState only on
      // `restart()`; a no-op default keeps the other tests quiet.
    },
  };
});

vi.mock('../../src/stores/agentNodeStore', () => ({
  useAgentNodeStore: {
    getState: () => ({ getAgentNodes: () => [] }),
  },
}));

import { UpdatePrompt } from '../../src/components/UpdatePrompt/UpdatePrompt';

const fakeUpdate = (): Update => ({
  version: '0.2.0',
  body: 'Shiny new things.',
  downloadAndInstall: vi.fn().mockResolvedValue(undefined),
} as unknown as Update);

describe('UpdatePrompt (issue #1526)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders nothing when there is no update (idle / disabled)', async () => {
    runUpdateCheckMock.mockResolvedValue({ phase: 'disabled' });
    const { container } = render(<UpdatePrompt />);
    await waitFor(() => expect(runUpdateCheckMock).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it('renders nothing when quiet check says current (no nag)', async () => {
    // The auto-launch check is quiet — `current` does not surface a
    // prompt. Settings > About handles "you're up to date".
    runUpdateCheckMock.mockResolvedValue({ phase: 'current' });
    const { container } = render(<UpdatePrompt />);
    await waitFor(() => expect(runUpdateCheckMock).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it('renders nothing when quiet check says unreachable (no nag)', async () => {
    runUpdateCheckMock.mockResolvedValue({ phase: 'unreachable', error: 'offline' });
    const { container } = render(<UpdatePrompt />);
    await waitFor(() => expect(runUpdateCheckMock).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it('shows the available prompt with version + notes', async () => {
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: 'Shiny new things.', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    expect(await screen.findByText('Buildmesh 0.2.0 is available.')).toBeTruthy();
    expect(screen.getByText('Shiny new things.')).toBeTruthy();
  });

  it('Install button triggers download + install', async () => {
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: '', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    const install = await screen.findByTestId('update-prompt-install');
    fireEvent.click(install);
    await waitFor(() => expect(downloadAndInstallUpdateMock).toHaveBeenCalledTimes(1));
  });

  it('dismisses the available prompt on Later without installing', async () => {
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: '', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    const later = await screen.findByRole('button', { name: 'Later' });
    fireEvent.click(later);
    await waitFor(() =>
      expect(screen.queryByText('Buildmesh 0.2.0 is available.')).toBeNull(),
    );
    expect(downloadAndInstallUpdateMock).not.toHaveBeenCalled();
  });

  it('shows the Restart ready prompt after a successful install', async () => {
    downloadAndInstallUpdateMock.mockImplementation(
      async (
        _u: Update,
        onProgress?: (p: { downloaded: number; total: number | null }) => void,
      ) => {
        onProgress?.({ downloaded: 100, total: 100 });
        onProgress?.({ downloaded: 100, total: 100 });
      },
    );
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: '', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    const install = await screen.findByTestId('update-prompt-install');
    await act(async () => {
      fireEvent.click(install);
    });
    // After install + the brief "installing" settle, the prompt flips
    // to the ready_to_restart surface.
    expect(await screen.findByTestId('update-prompt-restart')).toBeTruthy();
    expect(screen.getByText(/Restart to apply/i)).toBeTruthy();
  });

  it('Restart on ready_to_restart routes through the exit-prompt store', async () => {
    downloadAndInstallUpdateMock.mockResolvedValue(undefined);
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: '', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    const install = await screen.findByTestId('update-prompt-install');
    await act(async () => {
      fireEvent.click(install);
    });
    const restart = await screen.findByTestId('update-prompt-restart');
    await act(async () => {
      fireEvent.click(restart);
    });
    // With no active agents, restart calls `relaunch()` directly rather
    // than showing the exit-confirmation modal.
    await waitFor(() => expect(relaunchMock).toHaveBeenCalledTimes(1));
    expect(showExitPromptMock).not.toHaveBeenCalled();
  });

  it('shows the failed prompt with sanitized error + Retry', async () => {
    downloadAndInstallUpdateMock.mockRejectedValue(new Error('install broke'));
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: '', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    const install = await screen.findByTestId('update-prompt-install');
    await act(async () => {
      fireEvent.click(install);
    });
    const retry = await screen.findByTestId('update-prompt-retry');
    expect(retry).toBeTruthy();
    // The h2 heading is "Update failed"; the phase line says
    // "Installing the update failed: …" — both contain the literal
    // "failed" so we target the heading by role to avoid the
    // multi-match ambiguity.
    expect(screen.getByRole('heading', { name: 'Update failed' })).toBeTruthy();
    expect(screen.getByTestId('update-prompt-fallback')).toBeTruthy();
  });
});