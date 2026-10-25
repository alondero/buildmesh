import { act, fireEvent, render, screen, waitFor, cleanup } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import UsageScreen, { UsageCard } from '../../src/mobile/screens/UsageScreen';
import { getUsage, isAuthError } from '../../src/mobile/api';
import type { MobileProviderUsage } from '../../src/types/generated/MobileProviderUsage';
vi.mock('../../src/mobile/api', () => ({ getUsage: vi.fn(), isAuthError: vi.fn() }));

const row: MobileProviderUsage = { name: 'Claude', meter: {
  provider: 'anthropic', usageTracked: true, cachedAt: 1_700_000_000,
  usage: { provider: 'anthropic', loggedIn: true, windows: [{ label: 'Weekly', usedPercent: 0, resetsAt: '2026-11-01T12:00:00Z' }],
    balance: { remaining: 0, monthlySpend: 12, currency: 'USD' }, meters: [{ state: 'unlimited' }], detail: 'Pro plan', error: null },
} };
beforeEach(() => { vi.mocked(getUsage).mockReset(); vi.mocked(isAuthError).mockReturnValue(false); });
afterEach(cleanup);

describe('mobile usage', () => {
  it('renders zero quota, wallet, explicit state, reset and last-known stamp', () => {
    render(<UsageCard row={row} />);
    expect(screen.getByText('0.0%')).toBeTruthy();
    expect(screen.getByText('USD 0.00')).toBeTruthy();
    expect(screen.getByText('USD 12.00')).toBeTruthy();
    expect(screen.getByText('Unlimited')).toBeTruthy();
    expect(screen.getByText(/Resets:/)).toBeTruthy();
    expect(screen.getByText(/Last known value/)).toBeTruthy();
  });
  it('keeps readings on failed forced refresh and retries successfully', async () => {
    vi.mocked(getUsage).mockResolvedValueOnce([row]).mockRejectedValueOnce(new Error('Desktop unavailable')).mockResolvedValueOnce([]);
    const back = vi.fn();
    render(<UsageScreen onBack={back} onAuthFailed={vi.fn()} />);
    await screen.findByText('Claude');
    fireEvent.click(screen.getByText('Refresh'));
    await screen.findByRole('alert');
    expect(screen.getByText('Claude')).toBeTruthy();
    expect(getUsage).toHaveBeenLastCalledWith(true, expect.any(AbortSignal));
    fireEvent.click(screen.getByText('Refresh'));
    await screen.findByText(/No usage meters available/);
    expect(screen.queryByRole('alert')).toBeNull();
    fireEvent.click(screen.getByLabelText('Back'));
    expect(back).toHaveBeenCalledOnce();
  });
  it('shows a recoverable cold failure and delegates rejected sessions', async () => {
    vi.mocked(getUsage).mockRejectedValue(new Error('Unavailable'));
    const auth = vi.fn();
    render(<UsageScreen onBack={vi.fn()} onAuthFailed={auth} />);
    await screen.findByText(/Could not load usage/);
    vi.mocked(isAuthError).mockReturnValue(true);
    fireEvent.click(screen.getByText('Refresh'));
    await waitFor(() => expect(auth).toHaveBeenCalledOnce());
  });
  it('cancels a manual refresh when leaving the page', async () => {
    let resolve!: (rows: MobileProviderUsage[]) => void;
    vi.mocked(getUsage).mockResolvedValueOnce([row]).mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    const auth = vi.fn();
    const view = render(<UsageScreen onBack={vi.fn()} onAuthFailed={auth} />);
    await screen.findByText('Claude');
    fireEvent.click(screen.getByText('Refresh'));
    const signal = vi.mocked(getUsage).mock.calls[1][1]!;
    view.unmount();
    expect(signal.aborted).toBe(true);
    await act(async () => resolve([]));
    expect(auth).not.toHaveBeenCalled();
  });
});
