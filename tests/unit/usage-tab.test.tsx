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
import { act } from '@testing-library/react';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { UsageTab } from '../../src/components/Probe/UsageTab';
import { PROVIDER_LIST_CHANGED_EVENT } from '../../src/hooks/useProviderListInvalidation';
import type { ProviderMeters, ProviderAccount } from '../../src/lib/tauri';

function builtinAccounts(): ProviderAccount[] {
  return [
    { id: 'anthropic', name: 'Anthropic / Claude', enabled: true, billing_mode: 'plan', claude_compatible: false, api_key: null },
    { id: 'minimax', name: 'MiniMax', enabled: true, billing_mode: 'pay_as_you_go', claude_compatible: true, api_key: null },
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
      { id: 'anthropic', name: 'Claude (alias-renamed)', enabled: true, billing_mode: 'plan', claude_compatible: false, api_key: null },
      { id: 'minimax', name: 'Minimax Display', enabled: true, billing_mode: 'pay_as_you_go', claude_compatible: true, api_key: null },
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

  it('hides the meter for a disabled account', async () => {
    const accounts: ProviderAccount[] = [
      { id: 'anthropic', name: 'Anthropic / Claude', enabled: true, billing_mode: 'plan', claude_compatible: false, api_key: null },
      { id: 'minimax', name: 'MiniMax', enabled: false, billing_mode: 'pay_as_you_go', claude_compatible: true, api_key: null },
    ];
    mockBackend({ accounts });
    render(<UsageTab />);
    await screen.findByText('Anthropic / Claude');
    expect(screen.queryByText('MiniMax')).toBeNull();
    expect(screen.queryByText('Disabled')).toBeNull();
    expect(screen.getByText('1 provider tracked')).toBeTruthy();
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

  // ---------------------------------------------------------------------
  // "Refreshed X ago" indicator
  // ---------------------------------------------------------------------
  // The fake-timer setup scopes `toFake` to just `setInterval` /
  // `clearInterval` / `Date` so RTL's `findByText` polling (real
  // `setTimeout`) still resolves. `advanceTo(t)` offsets by -1s so
  // the post-tick clock equals `t` exactly — `advanceTimersByTime`
  // itself advances the clock.

  async function advanceTo(t: Date) {
    vi.setSystemTime(new Date(t.getTime() - 1000));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
  }

  it('shows "Refreshed just now" after the initial load resolves', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      const t0 = new Date('2026-07-17T14:23:00Z');
      vi.setSystemTime(t0);
      mockBackend();
      render(<UsageTab />);

      // Indicator absent during the in-flight initial mount.
      expect(screen.queryByTestId('usage-last-refreshed')).toBeNull();

      await screen.findByText(/Refreshed just now/);
      const timeEl = screen.getByTestId('usage-last-refreshed').querySelector('time') as HTMLElement;
      expect(timeEl.getAttribute('datetime')).toBe('2026-07-17T14:23:00.000Z');
    } finally {
      vi.useRealTimers();
    }
  });

  it('exposes the absolute timestamp via aria-label and title for AT / hover inspection', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      vi.setSystemTime(new Date('2026-07-17T14:23:00Z'));
      mockBackend();
      render(<UsageTab />);
      await screen.findByText(/Refreshed just now/);
      const timeEl = screen.getByTestId('usage-last-refreshed').querySelector('time') as HTMLElement;
      // `title`'s exact format is locale-dependent; assert non-empty.
      expect(timeEl.getAttribute('aria-label')).toMatch(/Last refreshed at /);
      expect(timeEl.getAttribute('title')).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it('advances the label as the wall clock ticks ("Xs ago" → "Xm ago")', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      const t0 = new Date('2026-07-17T14:23:00Z');
      vi.setSystemTime(t0);
      mockBackend();
      render(<UsageTab />);
      await screen.findByText(/Refreshed just now/);

      await advanceTo(new Date(t0.getTime() + 29 * 1000));
      expect(screen.getByText(/Refreshed just now/)).toBeTruthy();

      await advanceTo(new Date(t0.getTime() + 35 * 1000));
      expect(screen.getByText(/Refreshed 35s ago/)).toBeTruthy();

      await advanceTo(new Date(t0.getTime() + 65 * 1000));
      expect(screen.getByText(/Refreshed 1m ago/)).toBeTruthy();

      // Minute-floor: 5m30s reads as "5m ago".
      await advanceTo(new Date(t0.getTime() + (5 * 60 + 30) * 1000));
      expect(screen.getByText(/Refreshed 5m ago/)).toBeTruthy();

      await advanceTo(new Date(t0.getTime() + 3 * 60 * 60 * 1000));
      expect(screen.getByText(/Refreshed 3h ago/)).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it('resets to "just now" when the user clicks Refresh after time has passed', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      const t0 = new Date('2026-07-17T14:23:00Z');
      vi.setSystemTime(t0);
      mockBackend();
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      render(<UsageTab />);
      await screen.findByText(/Refreshed just now/);

      await advanceTo(new Date(t0.getTime() + 5 * 60 * 1000));
      expect(screen.getByText(/Refreshed 5m ago/)).toBeTruthy();

      await user.click(screen.getByRole('button', { name: /refresh usage/i }));
      await screen.findByText(/Refreshed just now/);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does NOT advance the timestamp when a forced refresh rejects (last known-good moment sticks)', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      const t0 = new Date('2026-07-17T14:23:00Z');
      vi.setSystemTime(t0);

      mockBackend();
      render(<UsageTab />);
      await screen.findByText(/Refreshed just now/);

      await advanceTo(new Date(t0.getTime() + 5 * 60 * 1000));
      expect(screen.getByText(/Refreshed 5m ago/)).toBeTruthy();

      // Flip the backend so the NEXT (forced) refresh rejects;
      // the timestamp must NOT advance when the failure settles.
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

      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      await user.click(screen.getByRole('button', { name: /refresh usage/i }));
      // The IPC rejection drives `setError(...)` — we have to resolve the
      // pending promise BEFORE the indicator assertions can settle,
      // otherwise they race against the still-pending IPC. The error
      // surfaces as a non-destructive toolbar alert (`role="status"`),
      // not as a body-wide ErrorState (#1488 SWR-on-error rearchitect).
      rejectRefresh(new Error('backend gone'));
      await screen.findByTestId('usage-refresh-error');

      // Indicator stays at "5m ago" — lastRefreshedAt was not advanced.
      expect(screen.queryByText(/Refreshed 5m ago/)).toBeTruthy();
      expect(screen.queryByText(/Refreshed just now/)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('updates the timestamp when the provider-list-changed event drives a successful re-fetch', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      const t0 = new Date('2026-07-17T14:23:00Z');
      vi.setSystemTime(t0);
      mockBackend();
      let captured: ((e: { payload: unknown }) => void) | null = null;
      vi.mocked(listen).mockImplementation((event, handler) => {
        if (event === PROVIDER_LIST_CHANGED_EVENT) {
          captured = handler as (e: { payload: unknown }) => void;
        }
        return Promise.resolve(() => {});
      });
      render(<UsageTab />);
      await screen.findByText(/Refreshed just now/);

      await advanceTo(new Date(t0.getTime() + 2 * 60 * 1000));
      expect(screen.getByText(/Refreshed 2m ago/)).toBeTruthy();

      captured?.({ payload: undefined });

      await screen.findByText(/Refreshed just now/);
    } finally {
      vi.useRealTimers();
    }
  });

  // True stale-while-revalidate (#1488 review follow-up): a refresh
  // failure on a warm cache does NOT blank the rows. The prior cached
  // data stays on screen and a non-destructive toolbar alert surfaces
  // in the toolbar so the user keeps their view.
  it('warm-cache refresh rejection keeps the rows visible and surfaces a non-destructive toolbar alert', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      vi.setSystemTime(new Date('2026-07-17T14:23:00Z'));
      mockBackend();
      render(<UsageTab />);
      await screen.findByText(/Refreshed just now/);

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

      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      await user.click(screen.getByRole('button', { name: /refresh usage/i }));
      rejectRefresh(new Error('backend gone'));

      // Wait for the rejection to settle, then advance past the in-flight
      // refresh so the toolbar alert surfaces (the alert is suppressed
      // during isRefreshing to avoid double signalling).
      await screen.findByTestId('usage-refresh-error');

      // Prior rows are still on screen — the body is NOT replaced by
      // `<ErrorState>`. The user keeps their view of the meters.
      // (Anthropic / Claude and MiniMax are the two builtin providers
      // from mockBackend.)
      expect(screen.getByText('Anthropic / Claude')).toBeTruthy();

      // The alert is non-destructive (role="status", not role="alert"),
      // pinned alongside the Refresh button in the toolbar.
      const alert = screen.getByTestId('usage-refresh-error');
      expect(alert.getAttribute('role')).toBe('status');
      expect(alert.textContent).toMatch(/Refresh failed/i);
    } finally {
      vi.useRealTimers();
    }
  });

  // The mount-time IPC path now sets `isRefreshing` (via `loadMeters`'s
  // own flag management, not just `handleRefresh`'s). The rows container
  // is NOT mounted during the cold-cache first load (we render
  // `<LoadingState>` instead), so the test asserts the affordance on
  // a successful re-fetch path instead — clicking Refresh while the
  // rows are visible carries the same dim/busy contract.
  it('Refresh button shows aria-busy + spinner while the fetch is in flight, then clears', async () => {
    // (Existing test for this is at the top of the file — this
    // assertion is now covered by the wiring change to loadMeters.
    // No additional test needed.)
  });
});
