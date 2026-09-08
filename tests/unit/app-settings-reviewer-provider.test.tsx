/** Reviewer-provider settings coverage: hydration, filtering, persistence, and rollback. */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';

const tauriMocks = vi.hoisted(() => ({
  getAppPreferences: vi.fn(),
  listProviders: vi.fn(),
  getProviderAccounts: vi.fn(),
  getKeyedFirstClassCatalog: vi.fn(),
  getCoordinatorStatus: vi.fn(),
  listDeviceSessions: vi.fn(),
  getNetworkStatus: vi.fn(),
  setAppReviewerProvider: vi.fn(),
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
  setAppReviewerProvider: tauriMocks.setAppReviewerProvider,
}));

import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

const NO_OVERRIDE = '__no_override__';

function provider(id: string, label: string) {
  return {
    id,
    label,
    color: '#fff',
    icon: id,
    resumable: id !== 'terminal',
    harness_id: id,
    provider_id: null,
    is_proxied: false,
    group_key: id,
    capabilities: {
      harness_id: id,
      supports_resume: id !== 'terminal',
      auto_resume_on_startup: id !== 'terminal',
      requires_attention_hook: false,
      produces_readable_transcript: id !== 'terminal',
      supports_model_override: id !== 'terminal',
      supports_effort_override: false,
      supports_prefill: id !== 'terminal',
      is_plain_terminal: id === 'terminal',
      effort_control: { kind: 'none' },
      available_on: ['windows'],
    },
  };
}

describe('AppSettingsModal reviewer provider', () => {
  let storedReviewer: string | null;

  beforeEach(() => {
    storedReviewer = 'codex';
    tauriMocks.getAppPreferences.mockReset().mockImplementation(() =>
      Promise.resolve({
        default_provider: null,
        reviewer_provider: storedReviewer,
        naming_provider: null,
        autopilot_pool_size: null,
        worktree_directory: null,
        harness_defaults: {},
        provider_pairings: [],
        confirm_before_quit: true,
      }),
    );
    tauriMocks.listProviders.mockReset().mockResolvedValue([
      provider('anthropic', 'Anthropic'),
      provider('codex', 'Codex'),
      provider('terminal', 'Terminal'),
    ]);
    tauriMocks.getProviderAccounts.mockReset().mockResolvedValue([]);
    tauriMocks.getKeyedFirstClassCatalog.mockReset().mockResolvedValue([]);
    tauriMocks.getCoordinatorStatus.mockReset().mockResolvedValue({ enabled: false, has_token: false });
    tauriMocks.listDeviceSessions.mockReset().mockResolvedValue([]);
    tauriMocks.getNetworkStatus.mockReset().mockResolvedValue({
      lan_exposure_enabled: false,
      tls_active: false,
      exposed_interfaces: [],
    });
    tauriMocks.setAppReviewerProvider.mockReset().mockImplementation((value: string | null) => {
      storedReviewer = value;
      return Promise.resolve(undefined);
    });
  });

  async function renderModal() {
    render(<AppSettingsModal onClose={() => {}} />);
    return screen.findByRole('combobox', { name: 'Reviewer provider' });
  }

  it('hydrates the stored provider and filters out the terminal provider', async () => {
    const select = (await renderModal()) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('codex'));
    expect(within(select).getByRole('option', { name: 'Codex' })).toBeTruthy();
    expect(within(select).queryByRole('option', { name: 'Terminal' })).toBeNull();
  });

  it('persists a selected provider and null for source-agent fallback', async () => {
    const select = (await renderModal()) as HTMLSelectElement;

    fireEvent.change(select, { target: { value: 'anthropic' } });
    await waitFor(() =>
      expect(tauriMocks.setAppReviewerProvider).toHaveBeenCalledWith('anthropic'),
    );

    fireEvent.change(select, { target: { value: NO_OVERRIDE } });
    await waitFor(() =>
      expect(tauriMocks.setAppReviewerProvider).toHaveBeenLastCalledWith(null),
    );
  });

  it('rolls back the selection and shows the save error when persistence fails', async () => {
    tauriMocks.setAppReviewerProvider.mockRejectedValueOnce(new Error('settings write failed'));
    const select = (await renderModal()) as HTMLSelectElement;

    fireEvent.change(select, { target: { value: 'anthropic' } });
    await waitFor(() => expect(select.value).toBe('codex'));
    expect(await screen.findByText('settings write failed')).toBeTruthy();
  });
});
