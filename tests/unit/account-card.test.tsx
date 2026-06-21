import { describe, it, expect, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { AccountCard } from '../../src/components/AppSettings/AppSettingsModal';
import type { ProviderAccount, ProviderUsage } from '../../src/lib/tauri';

function account(over: Partial<ProviderAccount> = {}): ProviderAccount {
  return {
    id: 'anthropic',
    name: 'Anthropic / Claude',
    enabled: true,
    billing_mode: 'plan',
    api_key: null,
    base_url: null,
    models: [],
    ...over,
  };
}

function usage(over: Partial<ProviderUsage> = {}): ProviderUsage {
  return { provider: 'anthropic', loggedIn: true, windows: [], balance: null, detail: null, error: null, ...over };
}

describe('AccountCard (issue #537)', () => {
  it('renders percentage bars for a plan account', () => {
    render(
      <AccountCard
        account={account({ billing_mode: 'plan' })}
        usage={usage({ windows: [{ label: '5-hour', usedPercent: 41, resetsAt: null }] })}
        usageLoading={false}
        onSave={vi.fn()}
      />,
    );
    expect(screen.getByText('41.0%')).toBeTruthy();
    expect(screen.queryByText('Balance remaining')).toBeNull();
  });

  it('renders a cash-balance card for a pay-as-you-go account', () => {
    render(
      <AccountCard
        account={account({ id: 'minimax', name: 'MiniMax', billing_mode: 'pay_as_you_go' })}
        usage={usage({ provider: 'minimax', balance: { remaining: 12.34, monthlySpend: 1.5, currency: 'USD' } })}
        usageLoading={false}
        onSave={vi.fn()}
      />,
    );
    expect(screen.getByText('USD 12.34')).toBeTruthy();
    expect(screen.queryByText(/%$/)).toBeNull();
  });

  it('shows a placeholder for a pay-as-you-go account with no balance yet', () => {
    render(
      <AccountCard
        account={account({ id: 'minimax', name: 'MiniMax', billing_mode: 'pay_as_you_go' })}
        usage={usage({ provider: 'minimax', balance: null })}
        usageLoading={false}
        onSave={vi.fn()}
      />,
    );
    expect(screen.getByText('Balance unavailable')).toBeTruthy();
  });

  it('shows "Disabled" and no usage when the account is disabled', () => {
    render(
      <AccountCard account={account({ enabled: false })} usage={undefined} usageLoading={false} onSave={vi.fn()} />,
    );
    expect(screen.getByText('Disabled')).toBeTruthy();
  });

  it('flips the enable toggle through onSave', async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(<AccountCard account={account({ enabled: true })} usage={usage()} usageLoading={false} onSave={onSave} />);

    await user.click(screen.getByRole('checkbox', { name: /enable anthropic/i }));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(onSave.mock.calls[0][0]).toMatchObject({ id: 'anthropic', enabled: false });
  });

  it('hides Remove for a built-in account', async () => {
    const user = userEvent.setup();
    render(
      <AccountCard account={account({ id: 'anthropic' })} usage={usage()} usageLoading={false} onSave={vi.fn()} onRemove={vi.fn()} />,
    );
    await user.click(screen.getByRole('button', { name: /edit credentials/i }));
    expect(screen.queryByRole('button', { name: /remove/i })).toBeNull();
  });

  it('offers Remove for a custom account', async () => {
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        usage={usage({ provider: 'deepseek' })}
        usageLoading={false}
        onSave={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    await user.click(screen.getByRole('button', { name: /edit credentials/i }));
    expect(screen.getByRole('button', { name: /remove/i })).toBeTruthy();
  });
});
