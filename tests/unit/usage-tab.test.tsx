/**
 * Tests for `<UsageTab>` — the new Probe Panel tab body for usage
 * meters (issue #601).
 *
 * Pins:
 *   - the tab mounts and calls `get_provider_meters` once with
 *     `force_refresh: false`
 *   - each returned `ProviderMeters` renders as one read-only row,
 *     joined by id to `get_provider_accounts` for the name/icon
 *   - the tab-level Refresh button re-calls `get_provider_meters`
 *     with `force_refresh: true`
 *   - the tab never shows credential editor / enable toggle / Remove —
 *     those live on the Settings-side AccountCard
 *   - on a fresh fetch (loading=true), the panel renders a loading
 *     state, not the previous rows
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { UsageTab } from '../../src/components/Probe/UsageTab';
import { PROVIDER_LIST_CHANGED_EVENT } from '../../src/hooks/useProviderListInvalidation';
import type { ProviderMeters, ProviderAccount } from '../../src/lib/tauri';

const NO_TIERS = { default: null, small_fast: null, sonnet: null, opus: null, haiku: null };

function builtinAccounts(): ProviderAccount[] {
  return [
    { id: 'anthropic', name: 'Anthropic / Claude', enabled: true, billing_mode: 'plan', claude_compatible: false, api_key: null, base_url: null, model_tiers: NO_TIERS, models: [] },
    { id: 'minimax', name: 'MiniMax', enabled: true, billing_mode: 'pay_as_you_go', claude_compatible: true, api_key: null, base_url: null, model_tiers: NO_TIERS, models: [] },
  ];
}

function mockBackend(opts: {
  meters?: ProviderMeters[];
  accounts?: ProviderAccount[];
} = {}) {
  const accounts = opts.accounts ?? builtinAccounts();
  const meters = opts.meters ?? [
    { provider: 'anthropic', usageTracked: true, usage: { provider: 'anthropic', loggedIn: true, windows: [{ label: '5-hour', usedPercent: 42, resetsAt: null }], balance: null, detail: null, error: null } },
    { provider: 'minimax', usageTracked: true, usage: { provider: 'minimax', loggedIn: true, windows: [], balance: { remaining: 12.34, monthlySpend: 1.5, currency: 'USD' }, detail: null, error: null } },
  ];
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_provider_meters':
        return Promise.resolve(meters);
      case 'get_provider_accounts':
        return Promise.resolve(accounts);
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

describe('UsageTab (issue #601 ProbePanel usage tab)', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('fetches get_provider_meters on mount', async () => {
    const calls = mockBackend();
    render(<UsageTab />);
    await waitFor(() => expect(calls['get_provider_meters']).toBeTruthy());
    // Initial fetch is a non-forced read (#574: 5-min cache TTL on backend).
    expect(calls['get_provider_meters']![0]).toEqual({ forceRefresh: false });
  });

  it('renders one row per detection-gated meter', async () => {
    mockBackend();
    render(<UsageTab />);
    // Bars + balance from the fixture both surface.
    expect(await screen.findByText('42.0%')).toBeTruthy();
    expect(screen.getByText('USD 12.34')).toBeTruthy();
    expect(screen.getByText('Anthropic / Claude')).toBeTruthy();
    expect(screen.getByText('MiniMax')).toBeTruthy();
  });

  it('joins the meter to its account by id and uses the account name', async () => {
    // Two meters, two distinct names — the join is the visual contract.
    const accounts: ProviderAccount[] = [
      { id: 'anthropic', name: 'Claude (alias-renamed)', enabled: true, billing_mode: 'plan', claude_compatible: false, api_key: null, base_url: null, model_tiers: NO_TIERS, models: [] },
      { id: 'minimax', name: 'Minimax Display', enabled: true, billing_mode: 'pay_as_you_go', claude_compatible: true, api_key: null, base_url: null, model_tiers: NO_TIERS, models: [] },
    ];
    mockBackend({ accounts });
    render(<UsageTab />);
    expect(await screen.findByText('Claude (alias-renamed)')).toBeTruthy();
    expect(screen.getByText('Minimax Display')).toBeTruthy();
  });

  it('drops meters whose account has been removed (orphan rows)', async () => {
    // A meter for `agy` arrives but no account by that id exists → the
    // tab renders nothing for that row rather than a bare meter.
    mockBackend({
      meters: [
        { provider: 'agy', usageTracked: true, usage: null },
        ...builtinAccounts().map(a => ({ provider: a.id, usageTracked: true, usage: null })),
      ] as ProviderMeters[],
    });
    render(<UsageTab />);
    await screen.findByText('Anthropic / Claude');
    expect(screen.queryByText('Antigravity')).toBeNull();
  });

  it('forces a refresh when the Refresh button is clicked', async () => {
    const calls = mockBackend();
    const user = userEvent.setup();
    render(<UsageTab />);
    await screen.findByText('Anthropic / Claude');
    await user.click(screen.getByRole('button', { name: /refresh usage/i }));
    await waitFor(() => {
      const forceCalls = (calls['get_provider_meters'] ?? []).filter(
        (a) => (a as { forceRefresh: boolean }).forceRefresh === true,
      );
      expect(forceCalls.length).toBeGreaterThanOrEqual(1);
    });
  });

  // Without feedback on Refresh, a slow backend refresh leaves the user
  // staring at stale-looking rows with no signal anything is happening.
  it('Refresh button shows aria-busy + spinner while the fetch is in flight, then clears', async () => {
    let resolveRefresh!: (rows: ProviderMeters[]) => void;
    const refreshPending = new Promise<ProviderMeters[]>((res) => { resolveRefresh = res; });

    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      const a = args as { forceRefresh?: boolean } | undefined;
      if (cmd === 'get_provider_meters') {
        // Hold the forced-refresh call; the initial mount-fetch resolves fast.
        if (a?.forceRefresh === true) return refreshPending;
        return Promise.resolve([
          { provider: 'anthropic', usageTracked: true, usage: { provider: 'anthropic', loggedIn: true, windows: [{ label: '5-hour', usedPercent: 42, resetsAt: null }], balance: null, detail: null, error: null } },
        ]);
      }
      if (cmd === 'get_provider_accounts') return Promise.resolve(builtinAccounts());
      return Promise.resolve({});
    });

    const user = userEvent.setup();
    const { container } = render(<UsageTab />);
    await screen.findByText('Anthropic / Claude');

    const btn = screen.getByRole('button', { name: /refresh usage/i });
    const rows = screen.getByTestId('usage-rows');

    // Pre-click: idle — not busy, not disabled, no spinner.
    expect(btn.getAttribute('aria-busy')).not.toBe('true');
    expect(btn.hasAttribute('disabled')).toBe(false);
    expect(container.querySelector('button[aria-label="Refresh usage"] .animate-spin')).toBeNull();

    // Click and let the in-flight state render.
    await user.click(btn);

    // While pending: button is busy + disabled with an inline spinner;
    // the rows region carries aria-busy so AT users hear the update too.
    await waitFor(() => {
      expect(btn.getAttribute('aria-busy')).toBe('true');
      expect(btn.hasAttribute('disabled')).toBe(true);
    });
    expect(container.querySelector('button[aria-label="Refresh usage"] .animate-spin')).toBeTruthy();
    expect(rows.getAttribute('aria-busy')).toBe('true');

    // Resolve the in-flight refresh — counts must include the forced call.
    resolveRefresh([
      { provider: 'anthropic', usageTracked: true, usage: { provider: 'anthropic', loggedIn: true, windows: [{ label: '5-hour', usedPercent: 75, resetsAt: null }], balance: null, detail: null, error: null } },
    ]);

    // Post-resolve: button returns to idle, rows region clears.
    await waitFor(() => {
      expect(btn.getAttribute('aria-busy')).not.toBe('true');
      expect(btn.hasAttribute('disabled')).toBe(false);
    });
    expect(container.querySelector('button[aria-label="Refresh usage"] .animate-spin')).toBeNull();
    expect(rows.getAttribute('aria-busy')).not.toBe('true');
  });

  it('clears the refreshing state even when the refresh fetch rejects', async () => {
    // The safety-net `catch` in handleRefresh must not leak isRefreshing=true
    // on a backend failure — otherwise the button is stuck disabled and the
    // tab looks permanently broken until the user closes the probe.
    let rejectRefresh!: (err: Error) => void;
    const refreshPending = new Promise<ProviderMeters[]>((_res, rej) => { rejectRefresh = rej; });

    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      const a = args as { forceRefresh?: boolean } | undefined;
      if (cmd === 'get_provider_meters') {
        if (a?.forceRefresh === true) return refreshPending;
        return Promise.resolve([]);
      }
      if (cmd === 'get_provider_accounts') return Promise.resolve(builtinAccounts());
      return Promise.resolve({});
    });

    const user = userEvent.setup();
    render(<UsageTab />);
    await screen.findByText(/no usage meters available/i);

    const btn = screen.getByRole('button', { name: /refresh usage/i });
    await user.click(btn);

    await waitFor(() => {
      expect(btn.getAttribute('aria-busy')).toBe('true');
    });

    rejectRefresh(new Error('backend gone'));
    await waitFor(() => {
      expect(btn.getAttribute('aria-busy')).not.toBe('true');
      expect(btn.hasAttribute('disabled')).toBe(false);
    });
  });

  it('renders no edit-credentials / enable-toggle / Remove affordances (read-only)', async () => {
    mockBackend();
    render(<UsageTab />);
    await screen.findByText('Anthropic / Claude');
    expect(screen.queryByRole('button', { name: /edit credentials/i })).toBeNull();
    expect(screen.queryByRole('checkbox', { name: /enable/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /remove/i })).toBeNull();
  });

  it('renders an empty-state message when there are no meters at all', async () => {
    mockBackend({ meters: [] });
    render(<UsageTab />);
    expect(await screen.findByText(/no usage meters available/i)).toBeTruthy();
  });

  // Issue #601 review: cross-surface invalidation. When the user enables,
  // disables, or removes a provider in App Settings, the Rust backend emits
  // `provider-list-changed`. UsageTab must re-fetch so a toggled provider's
  // meter appears/disappears without a manual Refresh click. Regression:
  // before #601's review-fix pass, the tab only fetched on mount and stayed
  // stale forever until the user clicked Refresh.
  it('re-fetches meters when the provider-list-changed event fires', async () => {
    const calls = mockBackend();
    let captured: ((e: { payload: unknown }) => void) | null = null;
    vi.mocked(listen).mockImplementation((event, handler) => {
      if (event === PROVIDER_LIST_CHANGED_EVENT) {
        captured = handler as (e: { payload: unknown }) => void;
      }
      return Promise.resolve(() => {});
    });

    render(<UsageTab />);
    await screen.findByText('Anthropic / Claude');

    // Sanity: one initial mount-fetch with forceRefresh=false.
    const initialCalls = (calls['get_provider_meters'] ?? []).filter(
      (a) => (a as { forceRefresh: boolean }).forceRefresh === false,
    );
    expect(initialCalls.length).toBe(1);

    // Backend fires the cross-surface event (e.g. Settings upsert/remove).
    captured?.({ payload: undefined });

    await waitFor(() => {
      const totalCalls = calls['get_provider_meters'] ?? [];
      // The refetch is non-forced so the backend's 5-min cache can serve it.
      const nonForceCalls = totalCalls.filter(
        (a) => (a as { forceRefresh: boolean }).forceRefresh === false,
      );
      expect(nonForceCalls.length).toBeGreaterThanOrEqual(2);
    });
  });
});
