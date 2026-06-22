import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import type { ProviderAccount } from '../../src/lib/tauri';

const NO_TIERS = { default: null, small_fast: null, sonnet: null, opus: null, haiku: null };

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
        // Detection-gated rows drive which cards render. Both built-ins are
        // visible here (Claude Code "detected", MiniMax enabled) so both cards show.
        return Promise.resolve([
          { provider: 'anthropic', usageTracked: true, usage: null },
          { provider: 'minimax', usageTracked: true, usage: null },
        ]);
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'get_network_status':
        return Promise.resolve({ lan_exposure_enabled: false, port: 1992 });
      case 'upsert_provider_account':
        accounts.push((args as { account: ProviderAccount }).account);
        return Promise.resolve(undefined);
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
});
