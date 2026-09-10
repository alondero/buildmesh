/**
 * The in-app update prompt (issue #1526 + #1654 review).
 *
 * Phase-aware rendering — the modal only appears for `available` /
 * `downloading` / `installing` / `ready_to_restart` / `failed` and
 * exposes different actions for each. Quiet check phases (`idle`,
 * `checking`, `current`, `unreachable`, `restarting`) render nothing —
 * those are the Settings > About section's concern, except
 * `restarting` which is the brief "click registered, awaiting the
 * modal decision" window so the ExitConfirmationModal can stack
 * cleanly (review finding 4 follow-up).
 *
 * The hook's pure state machine is covered by `updater.test.ts` (lib)
 * and `useUpdateCheck.test.tsx` (store); this file exercises the
 * modal wiring.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import type { Update } from '@tauri-apps/plugin-updater';

const {
  runUpdateCheckMock,
  downloadUpdateMock,
  installUpdateMock,
  relaunchMock,
  requestExitMock,
} = vi.hoisted(() => ({
  runUpdateCheckMock: vi.fn(),
  downloadUpdateMock: vi.fn(),
  installUpdateMock: vi.fn().mockResolvedValue(undefined),
  relaunchMock: vi.fn().mockResolvedValue(undefined),
  requestExitMock: vi.fn().mockResolvedValue(true),
}));

vi.mock('../../src/lib/updater', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/updater')>();
  return {
    ...actual,
    runUpdateCheck: runUpdateCheckMock,
    downloadUpdate: downloadUpdateMock,
    installUpdate: installUpdateMock,
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
      getState: () => ({
        requestExit: requestExitMock,
      }),
    },
  };
});

vi.mock('../../src/stores/agentNodeStore', () => ({
  useAgentNodeStore: {
    getState: () => ({ getAgentNodes: () => [] }),
  },
}));

import { UpdatePrompt } from '../../src/components/UpdatePrompt/UpdatePrompt';
import { resetUpdaterStateForTests } from '../../src/stores/updaterStore';

const fakeUpdate = (): Update => ({
  version: '0.2.0',
  body: 'Shiny new things.',
  downloadAndInstall: vi.fn().mockResolvedValue(undefined),
} as unknown as Update);

describe('UpdatePrompt (issue #1526 + #1654 review)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetUpdaterStateForTests();
    requestExitMock.mockResolvedValue(true);
  });

  it('renders nothing when the updater is disabled in this build', async () => {
    runUpdateCheckMock.mockResolvedValue({ phase: 'disabled' });
    const { container } = render(<UpdatePrompt />);
    await waitFor(() => expect(runUpdateCheckMock).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it('renders nothing when quiet check says current (no nag)', async () => {
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

  it('Install button triggers download + install (split functions)', async () => {
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: '', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    const install = await screen.findByTestId('update-prompt-install');
    fireEvent.click(install);
    await waitFor(() => expect(downloadUpdateMock).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(installUpdateMock).toHaveBeenCalledTimes(1));
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
    expect(downloadUpdateMock).not.toHaveBeenCalled();
  });

  it('shows the Restart ready prompt after a successful install', async () => {
    downloadUpdateMock.mockImplementation(
      async (
        _u: Update,
        onProgress?: (p: { downloaded: number; total: number | null }) => void,
      ) => {
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
    expect(await screen.findByTestId('update-prompt-restart')).toBeTruthy();
    expect(screen.getByText(/Restart to apply/i)).toBeTruthy();
  });

  // Renamed per review style item — the pre-review name "routes
  // through the exit-prompt store" was a naming lie. With no active
  // agents, restart calls `relaunch()` directly rather than showing
  // the exit-confirmation modal. The exit-prompt store path is
  // covered by the `restart delegates to useExitPromptStore.requestExit`
  // test in `useUpdateCheck.test.tsx`.
  it('bypasses the prompt with no active agents (calls relaunch directly)', async () => {
    requestExitMock.mockResolvedValue(false);
    downloadUpdateMock.mockResolvedValue(undefined);
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
    await waitFor(() => expect(relaunchMock).toHaveBeenCalledTimes(1));
  });

  // Finding 5 — the ProgressPrompt has a real Cancel button during
  // download. The button is the focusable surface Tab needs (the
  // pre-review panel had zero focusables — a keyboard trap).
  it('ProgressPrompt exposes a Cancel download button', async () => {
    let resolveDownload!: () => void;
    downloadUpdateMock.mockImplementation(
      async (
        _u: Update,
        onProgress?: (p: { downloaded: number; total: number | null }) => void,
      ) => {
        onProgress?.({ downloaded: 50, total: 100 });
        await new Promise<void>((r) => { resolveDownload = r; });
      },
    );
    runUpdateCheckMock.mockResolvedValue({
      phase: 'available',
      update: fakeUpdate(),
      summary: { version: '0.2.0', notes: '', message: 'Buildmesh 0.2.0 is available.' },
    });
    render(<UpdatePrompt />);
    const install = await screen.findByTestId('update-prompt-install');
    act(() => fireEvent.click(install));
    const cancel = await screen.findByTestId('update-prompt-cancel');
    expect(cancel.textContent).toBe('Cancel download');
    // Clicking Cancel transitions to `failed { failedAt: 'download' }`.
    await act(async () => {
      fireEvent.click(cancel);
    });
    const retry = await screen.findByTestId('update-prompt-retry');
    expect(retry).toBeTruthy();
    // Heading still says "Update failed" but the body distinguishes
    // download vs install (finding 3).
    expect(screen.getByText(/Downloading the update failed/i)).toBeTruthy();
    // Drain the in-flight download so React commits settle.
    resolveDownload();
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
  });

  it('shows the failed prompt with sanitized error + Retry', async () => {
    installUpdateMock.mockRejectedValue(new Error('install broke'));
    downloadUpdateMock.mockResolvedValue(undefined);
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
    expect(screen.getByRole('heading', { name: 'Update failed' })).toBeTruthy();
    expect(screen.getByTestId('update-prompt-fallback')).toBeTruthy();
  });
});