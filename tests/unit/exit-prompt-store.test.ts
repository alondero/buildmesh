/**
 * Exit prompt store (issue #1501) — in-memory confirm preference with
 * fail-closed boot hydration, plus the prompt/exit state machine.
 *
 * The exit path is two-layered (issue #1501 regression, 2026-09-06):
 * the ACL-proof `exit_application` backend command runs first, the
 * webview-side `destroy()` IPC is the fallback. When BOTH layers fail
 * the failure must be user-visible (toast) and `exiting` resets so the
 * button can be retried — the original bug hid behind a console.warn.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act } from '@testing-library/react';

const windowApi = vi.hoisted(() => ({
  destroy: vi.fn(),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => windowApi,
}));

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

vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri')>()),
  getAppPreferences: tauriMocks.getAppPreferences,
  cancelWindowClose: tauriMocks.cancelWindowClose,
  exitApplication: tauriMocks.exitApplication,
}));

import { useExitPromptStore } from '../../src/stores/exitPromptStore';

beforeEach(() => {
  windowApi.destroy.mockReset().mockResolvedValue(undefined);
  tauriMocks.getAppPreferences.mockReset().mockResolvedValue({ confirm_before_quit: false });
  tauriMocks.cancelWindowClose.mockReset().mockResolvedValue(undefined);
  tauriMocks.exitApplication.mockReset().mockResolvedValue(undefined);
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
      pending: { activeCount: 1, nonResumable: [] },
    });
    useExitPromptStore.getState().keepWorking();
    expect(useExitPromptStore.getState().pending).toBeNull();
    expect(tauriMocks.cancelWindowClose).toHaveBeenCalledTimes(1);
  });

  it('keepWorking without a prompt does not call the backend', () => {
    useExitPromptStore.getState().keepWorking();
    expect(tauriMocks.cancelWindowClose).not.toHaveBeenCalled();
  });

  it('confirmExit prefers the ACL-proof exit_application command and skips the fallback on success', async () => {
    useExitPromptStore.setState({
      pending: { activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    expect(tauriMocks.exitApplication).toHaveBeenCalledTimes(1);
    expect(windowApi.destroy).not.toHaveBeenCalled();
    expect(useExitPromptStore.getState().exiting).toBe(true);
  });

  it('confirmExit falls back to webview destroy when exit_application fails', async () => {
    tauriMocks.exitApplication.mockRejectedValueOnce(new Error('command failed'));
    useExitPromptStore.setState({
      pending: { activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    expect(tauriMocks.exitApplication).toHaveBeenCalledTimes(1);
    expect(windowApi.destroy).toHaveBeenCalledTimes(1);
    expect(useExitPromptStore.getState().exiting).toBe(true);
    expect(toastMock.addToast).not.toHaveBeenCalled();
  });

  it('confirmExit resets the busy state and toasts when BOTH exit layers fail', async () => {
    tauriMocks.exitApplication.mockRejectedValueOnce(new Error('command failed'));
    windowApi.destroy.mockRejectedValueOnce(new Error('no window'));
    useExitPromptStore.setState({
      pending: { activeCount: 1, nonResumable: [] },
    });
    await act(async () => {
      await useExitPromptStore.getState().confirmExit();
    });
    expect(tauriMocks.exitApplication).toHaveBeenCalledTimes(1);
    expect(windowApi.destroy).toHaveBeenCalledTimes(1);
    expect(useExitPromptStore.getState().exiting).toBe(false);
    expect(toastMock.addToast).toHaveBeenCalledTimes(1);
    expect(toastMock.addToast.mock.calls[0][0]).toBe('Exit Buildmesh');
  });
});
