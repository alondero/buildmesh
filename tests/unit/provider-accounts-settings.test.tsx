import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { openSettingsPane } from '../utils/settings-panes';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import type { ProviderAccount } from '../../src/lib/tauri';

const NO_TIERS = { default: null, small_fast: null, sonnet: null, opus: null, fable: null, haiku: null };

function builtinAccounts(): ProviderAccount[] {
  return [
    { id: 'anthropic', name: 'Anthropic / Claude', enabled: true, billing_mode: 'plan', claude_compatible: false, api_key: null, base_url: null, model_tiers: NO_TIERS, models: [] },
    { id: 'minimax', name: 'MiniMax', enabled: true, billing_mode: 'pay_as_you_go', claude_compatible: true, api_key: null, base_url: null, model_tiers: NO_TIERS, models: [] },
  ];
}

function mockBackend() {
  const accounts = builtinAccounts();
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({ default_provider: null, minimax_api_key: null });
      case 'list_providers':
        return Promise.resolve([]);
      case 'get_provider_accounts':
        return Promise.resolve(accounts);
      case 'get_provider_meters':
        // The meters endpoint is still called (to keep cache warm) but the
        // meters are no longer RENDERED in the Settings modal — they live on
        // the Probe Panel's "Usage" tab (issue #601). This regression pins
        // that contract by serving rich meters and asserting nothing in the
        // modal surface renders them.
        return Promise.resolve([
          {
            provider: 'anthropic',
            usageTracked: true,
            usage: {
              provider: 'anthropic',
              loggedIn: true,
              windows: [{ label: '5-hour', usedPercent: 42, resetsAt: null }],
              balance: null,
              detail: null,
              error: null,
            },
          },
          {
            provider: 'minimax',
            usageTracked: true,
            usage: {
              provider: 'minimax',
              loggedIn: true,
              windows: [],
              balance: { remaining: 12.34, monthlySpend: 1.5, currency: 'USD' },
              detail: null,
              error: null,
            },
          },
        ]);
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'get_network_status':
        return Promise.resolve({
          lan_exposure_enabled: false,
          port: 1992,
          tls_active: false,
          exposed_interfaces: [],
        });
      case 'upsert_provider_account':
        accounts.push((args as { account: ProviderAccount }).account);
        return Promise.resolve(undefined);
      case 'list_device_sessions':
        return Promise.resolve([]);
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

describe('Accounts & Usage settings (issue #537)', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('renders a card per merged provider account', async () => {
    mockBackend();
    render(<AppSettingsModal onClose={() => {}} />);
    expect(await screen.findByText('Anthropic / Claude')).toBeTruthy();
    expect(screen.getByText('MiniMax')).toBeTruthy();
  });

  it('creates a custom Claude-compatible provider with a slugified id', async () => {
    const calls = mockBackend();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByText('Anthropic / Claude');
    await openSettingsPane('Providers');

    await user.click(screen.getByRole('button', { name: /add custom provider/i }));
    await user.type(screen.getByLabelText(/custom provider name/i), 'DeepSeek via Claude Code');
    await user.type(screen.getByLabelText(/custom provider base URL/i), 'https://api.deepseek.com/anthropic');
    await user.type(screen.getByLabelText(/custom provider API key/i), 'sk-deep');
    await user.click(screen.getByRole('button', { name: /add provider/i }));

    await waitFor(() => expect(calls['upsert_provider_account']).toBeTruthy());
    const sent = (calls['upsert_provider_account']![0] as { account: ProviderAccount }).account;
    expect(sent).toMatchObject({
      id: 'deepseek-via-claude-code',
      name: 'DeepSeek via Claude Code',
      enabled: true,
      billing_mode: 'pay_as_you_go',
      base_url: 'https://api.deepseek.com/anthropic',
      api_key: 'sk-deep',
    });
  });

  // Issue #601 — Usage Meters moved off the Settings modal and onto the
  // Probe Panel's "Usage" tab. The Settings modal now owns credentials
  // only; it must NOT render the bars or balances, even when the backend
  // returns fully populated meters. This pins the split — if a future
  // refactor accidentally re-mounts the meter section inside Settings,
  // these assertions go loud.
  it('does NOT render Usage Meters (issue #601 — meters moved off the Settings modal)', async () => {
    mockBackend();
    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByText('Anthropic / Claude');
    // Bars from the Anthropic 5-hour window must not appear in the modal.
    expect(screen.queryByText('42.0%')).toBeNull();
    // The MiniMax balance must not appear either.
    expect(screen.queryByText('USD 12.34')).toBeNull();
    expect(screen.queryByText('Balance remaining')).toBeNull();
  });

  it('does NOT render the meters-only Refresh button (issue #601 — refresh lives on the Usage tab)', async () => {
    mockBackend();
    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByText('Anthropic / Claude');
    // The Refresh button used to sit next to the "Providers" header; it
    // only existed to re-fetch usage. Without meters, there's nothing to
    // refresh here — the affordance moves to the Usage tab.
    expect(screen.queryByRole('button', { name: /^refresh$/i })).toBeNull();
  });
});
