/** Reviewer-provider settings coverage: hydration, filtering, persistence, and rollback. */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { openSettingsPane } from '../utils/settings-panes';

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
    // Reviewer provider now lives on the Providers pane.
    await openSettingsPane('Providers');
    return screen.findByRole('button', { name: 'Reviewer provider' });
  }

  it('hydrates the stored provider and filters out the terminal provider', async () => {
    const trigger = await renderModal();
    await waitFor(() => expect(trigger.textContent).toContain('Codex'));

    const user = userEvent.setup();
    await user.click(trigger);
    expect(await screen.findByRole('menuitem', { name: 'Anthropic' })).toBeTruthy();
    // The plain shell is absent entirely: it is not an agent at all.
    expect(screen.queryByRole('menuitem', { name: 'Terminal' })).toBeNull();
  });

  it('persists a selected provider and null for source-agent fallback', async () => {
    const user = userEvent.setup();
    const trigger = await renderModal();
    await waitFor(() => expect(trigger.textContent).toContain('Codex'));

    await user.click(trigger);
    await user.click(await screen.findByRole('menuitem', { name: 'Anthropic' }));
    await waitFor(() =>
      expect(tauriMocks.setAppReviewerProvider).toHaveBeenCalledWith('anthropic'),
    );

    await user.click(trigger);
    await user.click(await screen.findByRole('menuitem', { name: 'Source agent provider' }));
    await waitFor(() =>
      expect(tauriMocks.setAppReviewerProvider).toHaveBeenLastCalledWith(null),
    );
  });

  it('rolls back the selection and shows the save error when persistence fails', async () => {
    tauriMocks.setAppReviewerProvider.mockRejectedValueOnce(new Error('settings write failed'));
    const user = userEvent.setup();
    const trigger = await renderModal();
    await waitFor(() => expect(trigger.textContent).toContain('Codex'));

    await user.click(trigger);
    await user.click(await screen.findByRole('menuitem', { name: 'Anthropic' }));

    await waitFor(() => expect(trigger.textContent).toContain('Codex'));
    expect(await screen.findByText('settings write failed')).toBeTruthy();
  });

  it('offers a harness with no turn signal, but not as a pickable reviewer', async () => {
    tauriMocks.listProviders.mockResolvedValue([
      provider('anthropic', 'Anthropic'),
      provider('cline', 'Cline'),
      provider('freebuff', 'Freebuff'),
      provider('terminal', 'Terminal'),
    ]);
    const user = userEvent.setup();
    const trigger = await renderModal();
    await user.click(trigger);

    const freebuff = await screen.findByRole('menuitem', { name: 'Freebuff' });
    // Greying out (not hiding) keeps the limitation discoverable — the same
    // reason the node title-bar picker renders these rows disabled.
    expect(freebuff.getAttribute('aria-disabled')).toBe('true');
    expect(freebuff.textContent).toContain('no review support');
    expect(screen.getByRole('menuitem', { name: 'Anthropic' }).getAttribute('aria-disabled')).toBe('false');
    // Issue #1775: Cline carries a native attention hook now and is pickable.
    expect(screen.getByRole('menuitem', { name: 'Cline' }).getAttribute('aria-disabled')).toBe('false');
    // The plain shell is absent entirely: it is not an agent at all.
    expect(screen.queryByRole('menuitem', { name: 'Terminal' })).toBeNull();
  });
});
