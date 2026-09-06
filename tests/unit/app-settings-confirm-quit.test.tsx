/**
 * App Settings confirm-before-quit toggle (issue #1501).
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';

const tauriMocks = vi.hoisted(() => ({
  getAppPreferences: vi.fn(),
  listProviders: vi.fn(),
  getProviderAccounts: vi.fn(),
  getKeyedFirstClassCatalog: vi.fn(),
  getCoordinatorStatus: vi.fn(),
  listDeviceSessions: vi.fn(),
  getNetworkStatus: vi.fn(),
  setAppConfirmBeforeQuit: vi.fn(),
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri')>()),
  getAppPreferences: tauriMocks.getAppPreferences,
  listProviders: tauriMocks.listProviders,
  getProviderAccounts: tauriMocks.getProviderAccounts,
  getKeyedFirstClassCatalog: tauriMocks.getKeyedFirstClassCatalog,
  getCoordinatorStatus: tauriMocks.getCoordinatorStatus,
  listDeviceSessions: tauriMocks.listDeviceSessions,
  getNetworkStatus: tauriMocks.getNetworkStatus,
  setAppConfirmBeforeQuit: tauriMocks.setAppConfirmBeforeQuit,
}));

import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { useExitPromptStore } from '../../src/stores/exitPromptStore';

beforeEach(() => {
  useExitPromptStore.setState({ pending: null, exiting: false, confirmBeforeQuit: true });
  tauriMocks.getAppPreferences.mockReset().mockResolvedValue({
    default_provider: null,
    naming_provider: null,
    autopilot_pool_size: null,
    worktree_directory: null,
    harness_defaults: {},
    provider_pairings: [],
    confirm_before_quit: true,
  });
  tauriMocks.listProviders.mockReset().mockResolvedValue([]);
  tauriMocks.getProviderAccounts.mockReset().mockResolvedValue([]);
  tauriMocks.getKeyedFirstClassCatalog.mockReset().mockResolvedValue([]);
  tauriMocks.getCoordinatorStatus.mockReset().mockResolvedValue({ enabled: false, has_token: false });
  tauriMocks.listDeviceSessions.mockReset().mockResolvedValue([]);
  tauriMocks.getNetworkStatus.mockReset().mockResolvedValue({
    lan_exposure_enabled: false,
    tls_active: false,
    exposed_interfaces: [],
  });
  tauriMocks.setAppConfirmBeforeQuit.mockReset().mockResolvedValue(undefined);
});

async function renderModal() {
  render(<AppSettingsModal onClose={() => {}} />);
  await screen.findByRole('checkbox', {
    name: 'Confirm before quitting when agent sessions are active',
  });
}

describe('App Settings confirm-before-quit (issue #1501)', () => {
  it('renders checked by default from the preference', async () => {
    await renderModal();
    expect(
      screen.getByRole('checkbox', {
        name: 'Confirm before quitting when agent sessions are active',
      }),
    ).toHaveProperty('checked', true);
  });

  it('persists an opt-out toggle to the backend', async () => {
    await renderModal();
    const box = screen.getByRole('checkbox', {
      name: 'Confirm before quitting when agent sessions are active',
    });
    await act(async () => {
      fireEvent.click(box);
    });
    await waitFor(() =>
      expect(tauriMocks.setAppConfirmBeforeQuit).toHaveBeenCalledWith(false),
    );
  });

  it('rolls back when the save fails', async () => {
    tauriMocks.setAppConfirmBeforeQuit.mockRejectedValueOnce(new Error('disk full'));
    await renderModal();
    const box = screen.getByRole('checkbox', {
      name: 'Confirm before quitting when agent sessions are active',
    }) as HTMLInputElement;
    await act(async () => {
      fireEvent.click(box);
    });
    await waitFor(() => expect(box.checked).toBe(true));
  });
});
