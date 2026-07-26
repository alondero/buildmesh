/**
 * Tests for `<UsagePanel>` — the read-only per-provider Usage Meter
 * row on the Probe Panel's new "Usage" tab (issue #601).
 *
 * These cases are lifted from `account-card.test.tsx` because the meter
 * rendering moves out of `AccountCard` (which stays a settings-side
 * credential/editor card) and into a dedicated read-only surface. The
 * contracts preserved verbatim:
 *   - 0% renders as a real figure, not N/A (issue #537 regression)
 *   - a plan account renders % bars; pay-as-you-go renders a balance;
 *     a provider may carry BOTH and the row shows all of them
 *     (issue #574 AC3; also fixed the MiniMax payg-XOR bug)
 *   - "Usage not tracked" is the explicit state for a Generic provider
 *     (issue #574 AC4), not an empty gauge or misleading error
 *   - "Disabled" is the state when the underlying account is disabled
 *
 * New contracts added for the glanceable surface:
 *   - the row joins an account by id (so the name + icon appear with
 *     the meter, instead of the meter floating bare)
 *   - the Refresh button is wired (the tab owns refresh; the panel
 *     only renders the affordance — but a per-row refresh is also
 *     allowed via prop)
 *   - the panel has NO credential editor, enable toggle, or Remove
 *     button — those live on the Settings-side AccountCard.
 */

import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { UsagePanel, UsageBar, BalanceCard } from '../../src/components/AppSettings/UsageRender';
import type { ProviderAccount, ProviderMeters, ProviderUsage } from '../../src/lib/tauri';
import { isClaudeCompatibleId } from '../../src/lib/providerClassification';

// Mirror the backend's derivation (preferences::is_claude_compatible_id):
// every account except the self-authenticating built-ins is
// Claude-compatible. Used to drive the "No API key" vs "Not logged in"
// copy in the "not logged in" branch.
function account(over: Partial<ProviderAccount> = {}): ProviderAccount {
  const id = over.id ?? 'anthropic';
  return {
    id: 'anthropic',
    name: 'Anthropic / Claude',
    enabled: true,
    billing_mode: 'plan',
    claude_compatible: isClaudeCompatibleId(id),
    api_key: null,
    ...over,
  };
}

function usage(over: Partial<ProviderUsage> = {}): ProviderUsage {
  return { provider: 'anthropic', loggedIn: true, windows: [], balance: null, detail: null, error: null, ...over };
}

function meter(over: Partial<ProviderMeters> = {}): ProviderMeters {
  return { provider: 'anthropic', usageTracked: true, usage: null, ...over };
}

describe('UsageBar (extracted, was on AccountCard)', () => {
  it('renders 0% used as a real figure, not N/A', () => {
    render(<UsageBar window={{ label: 'Claude Sonnet 4.6 (Thinking)', usedPercent: 0, resetsAt: null }} />);
    expect(screen.getByText('0.0%')).toBeTruthy();
    expect(screen.queryByText('N/A')).toBeNull();
  });

  it('renders a known non-zero percentage', () => {
    render(<UsageBar window={{ label: 'Gemini 3.5 Flash (Medium)', usedPercent: 20, resetsAt: null }} />);
    expect(screen.getByText('20.0%')).toBeTruthy();
  });

  it('shows N/A only when usedPercent is genuinely unknown (null)', () => {
    render(<UsageBar window={{ label: 'Unknown', usedPercent: null, resetsAt: null }} />);
    expect(screen.getByText('N/A')).toBeTruthy();
  });
});

describe('BalanceCard (extracted, was on AccountCard)', () => {
  it('renders remaining balance and monthly spend with the currency', () => {
    render(<BalanceCard balance={{ remaining: 42.5, monthlySpend: 7.25, currency: 'USD' }} />);
    expect(screen.getByText('USD 42.50')).toBeTruthy();
    expect(screen.getByText('USD 7.25')).toBeTruthy();
    expect(screen.getByText('Balance remaining')).toBeTruthy();
    expect(screen.getByText('Spent this month')).toBeTruthy();
  });

  it('omits the spend row when monthlySpend is null', () => {
    render(<BalanceCard balance={{ remaining: 100, monthlySpend: null, currency: 'CNY' }} />);
    expect(screen.getByText('CNY 100.00')).toBeTruthy();
    expect(screen.queryByText('Spent this month')).toBeNull();
  });
});

describe('UsagePanel (issue #601 read-only surface)', () => {
  it('renders percentage bars for a plan account', () => {
    render(
      <UsagePanel
        account={account({ billing_mode: 'plan' })}
        meter={meter({ usage: usage({ windows: [{ label: '5-hour', usedPercent: 41, resetsAt: null }] }) })}
      />,
    );
    expect(screen.getByText('41.0%')).toBeTruthy();
    expect(screen.queryByText('Balance remaining')).toBeNull();
  });

  it('renders a cash-balance card for a pay-as-you-go account', () => {
    render(
      <UsagePanel
        account={account({ id: 'minimax', name: 'MiniMax', billing_mode: 'pay_as_you_go' })}
        meter={meter({
          provider: 'minimax',
          usage: usage({ provider: 'minimax', balance: { remaining: 12.34, monthlySpend: 1.5, currency: 'USD' } }),
        })}
      />,
    );
    expect(screen.getByText('USD 12.34')).toBeTruthy();
    expect(screen.queryByText(/%$/)).toBeNull();
  });

  it('shows "No usage data" when a logged-in account has no meters yet', () => {
    // Meters render by what's present, not by billing mode (#574): no
    // windows and no balance → a neutral placeholder.
    render(
      <UsagePanel
        account={account({ id: 'minimax', name: 'MiniMax', billing_mode: 'pay_as_you_go' })}
        meter={meter({ provider: 'minimax', usage: usage({ provider: 'minimax', balance: null }) })}
      />,
    );
    expect(screen.getByText('No usage data')).toBeTruthy();
  });

  it('renders both quota windows and a cash balance together (#574 AC3)', () => {
    render(
      <UsagePanel
        account={account({ id: 'anthropic' })}
        meter={meter({
          usage: usage({
            windows: [{ label: '5-hour', usedPercent: 41, resetsAt: null }],
            balance: { remaining: 12.34, monthlySpend: null, currency: 'USD' },
          }),
        })}
      />,
    );
    expect(screen.getByText('41.0%')).toBeTruthy();
    expect(screen.getByText('USD 12.34')).toBeTruthy();
  });

  it('shows "Usage not tracked" for a Generic provider (#574 AC4)', () => {
    render(
      <UsagePanel
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        meter={meter({ provider: 'deepseek', usage: null, usageTracked: false })}
      />,
    );
    expect(screen.getByText('Usage not tracked')).toBeTruthy();
    expect(screen.queryByText('Unable to load usage data')).toBeNull();
  });

  it('shows "Disabled" when the account is disabled', () => {
    render(
      <UsagePanel
        account={account({ enabled: false })}
        meter={meter({ usage: null })}
      />,
    );
    expect(screen.getByText('Disabled')).toBeTruthy();
  });

  it('shows "No API key" for a keyed-but-logged-out provider', () => {
    render(
      <UsagePanel
        account={account({ id: 'minimax', name: 'MiniMax', billing_mode: 'pay_as_you_go' })}
        meter={meter({
          provider: 'minimax',
          usage: usage({ provider: 'minimax', loggedIn: false }),
        })}
      />,
    );
    expect(screen.getByText('No API key')).toBeTruthy();
    expect(screen.getByText(/Enter an API key for MiniMax above/i)).toBeTruthy();
  });

  it('shows "Not logged in" for a native-but-logged-out provider', () => {
    render(
      <UsagePanel
        account={account({ id: 'anthropic' })}
        meter={meter({ usage: usage({ loggedIn: false }) })}
      />,
    );
    expect(screen.getByText('Not logged in')).toBeTruthy();
    expect(screen.getByText(/Run the Anthropic \/ Claude CLI login first/i)).toBeTruthy();
  });

  it('surfaces a usage.error as a red message', () => {
    render(
      <UsagePanel
        account={account({ id: 'anthropic' })}
        meter={meter({ usage: usage({ error: 'Token expired' }) })}
      />,
    );
    expect(screen.getByText('Token expired')).toBeTruthy();
  });

  it('shows the provider name next to the meter (the glance contract)', () => {
    render(
      <UsagePanel
        account={account({ id: 'anthropic', name: 'Anthropic / Claude' })}
        meter={meter({ usage: usage({ windows: [{ label: '5-hour', usedPercent: 10, resetsAt: null }] }) })}
      />,
    );
    expect(screen.getByText('Anthropic / Claude')).toBeTruthy();
  });

  it('does NOT render an edit-credentials button (read-only surface)', () => {
    render(
      <UsagePanel
        account={account({ id: 'kimi', name: 'Kimi', billing_mode: 'pay_as_you_go' })}
        meter={meter({ provider: 'kimi', usage: usage({ provider: 'kimi' }) })}
      />,
    );
    // The Settings-side AccountCard keeps the credentials editor; the glance
    // surface (#601) is read-only.
    expect(screen.queryByRole('button', { name: /edit credentials/i })).toBeNull();
    expect(screen.queryByRole('checkbox', { name: /enable/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /remove/i })).toBeNull();
  });

  it('calls onRefresh when the row-level Refresh button is clicked', async () => {
    const onRefresh = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <UsagePanel
        account={account({ id: 'anthropic' })}
        meter={meter({ usage: usage() })}
        onRefresh={onRefresh}
      />,
    );
    await user.click(screen.getByRole('button', { name: /refresh usage for/i }));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });
});
