/**
 * Exit prompt store (issue #1501) — in-memory confirm preference with
 * fail-closed boot hydration, plus the prompt/exit state machine.
 *
 * The confirmed exit is a single backend lifecycle command
 * (`exit_application`). Its failure path is the interesting boundary: the
 * app is still running, so the expected-exit marking must be retracted,
 * the user must see a toast, and `exiting` must reset for retry.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act } from '@testing-library/react';

const toastMock = vi.hoisted(() => ({
  addToast: vi.fn(),
}));

vi.mock('../../src/stores/toastStore', () => ({
  addToast: toastMock.addToast,
}));

const tauriMocks = vi.hoisted(() => ({
  getAppPreferences: vi.fn(),
  cancelWindowClose: vi.fn(),
  exitApplication: vi.fn(),
}));

const { relaunchMock } = vi.hoisted(() => ({
  relaunchMock: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: relaunchMock,
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri')>()),
  getAppPreferences: tauriMocks.getAppPreferences,
  cancelWindowClose: tauriMocks.cancelWindowClose,
  exitApplication: tauriMocks.exitApplication,
}));

import { useExitPromptStore } from '../../src/stores/exitPromptStore';

beforeEach(() => {
  tauriMocks.getAppPreferences.mockReset().mockResolvedValue({ confirm_before_quit: false });
  tauriMocks.cancelWindowClose.mockReset().mockResolvedValue(undefined);
  tauriMocks.exitApplication.mockReset().mockResolvedValue(undefined);
  relaunchMock.mockReset().mockResolvedValue(undefined);
  toastMock.addToast.mockReset();
  useExitPromptStore.setState({ pending: null, exiting: false, confirmBeforeQuit: true });
});

describe('useExitPromptStore (issue #1501)', () => {
  it('defaults to prompting before boot hydration lands', () => {
    expect(useExitPromptStore.getState().confirmBeforeQuit).toBe(true);
  });

  it('hydrates the stored preference on success', async () => {
    await act(async () => {
      await useExitPromptStore.getState().initConfirmBeforeQuit();
    });
    expect(useExitPromptStore.getState().confirmBeforeQuit).toBe(false);
  });

  it('keeps the fail-closed default when hydration fails', async () => {
    tauriMocks.getAppPreferences.mockRejectedValueOnce(new Error('ipc down'));
    await act(async () => {
      await useExitPromptStore.getState().initConfirmBeforeQuit();
    });
    expect(useExitPromptStore.getState().confirmBeforeQuit).toBe(true);
  });

  it('keepWorking clears the prompt and retracts the expected-exit marking', () => {
    useExitPromptStore.setState({
      pending: { mode: 'window-close', activeCount: 1, nonResumable: [] },
    });
    useExitPromptStore.getState().keepWorking();
    expect(useExitPromptStore.getState().pending).toBeNull();
    expect(tauriMocks.cancelWindowClose).toHaveBeenCalledTimes(1);
  });

  it('keepWorking without a prompt does not call the backend', () => {
    useExitPromptStore.getState().keepWorking();
    expect(tauriMocks.cancelWindowClose).not.toHaveBeenCalled();
  });

  it('confirmExit hands shutdown to the backend lifecycle command', async () => {
    useExitPromptStore.setState({
      pending: { mode: 'window-close', activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    expect(tauriMocks.exitApplication).toHaveBeenCalledTimes(1);
    // Shutdown is in flight — no retract, no toast, no retry state.
    expect(tauriMocks.cancelWindowClose).not.toHaveBeenCalled();
    expect(toastMock.addToast).not.toHaveBeenCalled();
    expect(useExitPromptStore.getState().exiting).toBe(true);
  });

  it('a failed exit retracts the expected-exit marking, toasts, and resets for retry', async () => {
    tauriMocks.exitApplication.mockRejectedValueOnce(new Error('command failed'));
    useExitPromptStore.setState({
      pending: { mode: 'window-close', activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    // The app is still running, so the backend's eager expected-exit
    // marking (recorded on CloseRequested) must be retracted — otherwise a
    // later real crash is misclassified as intentional and the watchdog
    // skips the auto-relaunch.
    expect(tauriMocks.cancelWindowClose).toHaveBeenCalledTimes(1);
    expect(toastMock.addToast).toHaveBeenCalledTimes(1);
    expect(toastMock.addToast.mock.calls[0][0]).toBe('Exit Buildmesh');
    expect(useExitPromptStore.getState().exiting).toBe(false);
  });

  it('a failed retract never blocks the toast and retry reset', async () => {
    tauriMocks.exitApplication.mockRejectedValueOnce(new Error('command failed'));
    tauriMocks.cancelWindowClose.mockRejectedValueOnce(new Error('ipc down'));
    useExitPromptStore.setState({
      pending: { mode: 'window-close', activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    expect(toastMock.addToast).toHaveBeenCalledTimes(1);
    expect(useExitPromptStore.getState().exiting).toBe(false);
  });

  it('update-restart mode dispatches relaunch, not exitApplication (issue #1526)', async () => {
    // The updater route must NOT call exit_application — that ends the
    // process without starting the staged binary. The plugin's
    // relaunch() swaps the binary on top of the running process.
    useExitPromptStore.setState({
      pending: { mode: 'update-restart', activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    expect(relaunchMock).toHaveBeenCalledTimes(1);
    expect(tauriMocks.exitApplication).not.toHaveBeenCalled();
    // Plugin handles the swap on success — no retract / toast needed.
    expect(tauriMocks.cancelWindowClose).not.toHaveBeenCalled();
    expect(toastMock.addToast).not.toHaveBeenCalled();
  });

  it('update-restart failure shows a distinct toast (issue #1526)', async () => {
    relaunchMock.mockRejectedValueOnce(new Error('plugin failed'));
    useExitPromptStore.setState({
      pending: { mode: 'update-restart', activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    expect(toastMock.addToast).toHaveBeenCalledTimes(1);
    // Distinct toast title so the user can tell which flow interrupted.
    expect(toastMock.addToast.mock.calls[0][0]).toBe('Restart failed');
    expect(useExitPromptStore.getState().exiting).toBe(false);
  });
});
