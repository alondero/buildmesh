import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { BalanceCard } from '../../src/components/AppSettings/UsageRender';

describe('BalanceCard', () => {
  it('renders remaining balance and monthly spend with the currency (issue #537)', () => {
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

  // Hiding zero balances is a UX call: a literal "0.00 credits" line is
  // noise on a fresh wallet (no prior non-zero reading to compare
  // against). The bar uses `remaining !== 0` so -0 collapses to 0 (JS
  // equality) AND any negative remaining — including overdrawn wallets
  // — is surfaced, because debt is exactly when the user needs to see
  // the number. "Spent this month" stays even when remaining is zero.
  it('hides the "Balance remaining" row when remaining is zero and there is no spend', () => {
    render(<BalanceCard balance={{ remaining: 0, monthlySpend: null, currency: 'credits' }} />);
    expect(screen.queryByText('Balance remaining')).toBeNull();
    expect(screen.queryByText(/0\.00 credits/)).toBeNull();
  });

  it('keeps monthly spend but hides "Balance remaining" when remaining is zero and there is spend', () => {
    render(<BalanceCard balance={{ remaining: 0, monthlySpend: 12.34, currency: 'credits' }} />);
    expect(screen.queryByText('Balance remaining')).toBeNull();
    expect(screen.getByText('Spent this month')).toBeTruthy();
    expect(screen.getByText('credits 12.34')).toBeTruthy();
  });

  it('shows "Balance remaining" for any positive value (the useful case)', () => {
    render(<BalanceCard balance={{ remaining: 0.01, monthlySpend: null, currency: 'credits' }} />);
    expect(screen.getByText('Balance remaining')).toBeTruthy();
    expect(screen.getByText('credits 0.01')).toBeTruthy();
  });

  it('surfaces a negative remaining (overdrawn wallet — debt is informative)', () => {
    render(<BalanceCard balance={{ remaining: -5.25, monthlySpend: 10, currency: 'credits' }} />);
    expect(screen.getByText('Balance remaining')).toBeTruthy();
    expect(screen.getByText('credits -5.25')).toBeTruthy();
    expect(screen.getByText('Spent this month')).toBeTruthy();
  });

  it('collapses -0 to 0 (JS equality) — no row is rendered', () => {
    // -0 === 0 in JavaScript so the strict-not-equal predicate hides it.
    // This guards against any future change to the predicate that might
    // distinguish the sign bit.
    render(<BalanceCard balance={{ remaining: -0, monthlySpend: null, currency: 'credits' }} />);
    expect(screen.queryByText('Balance remaining')).toBeNull();
  });

  it('returns null when remaining is exactly zero and there is no monthly spend', () => {
    // The whole component returns null so the parent doesn't render an
    // empty <div> with stray margin. Pin this so a future refactor that
    // falls back to a wrapper div is caught.
    const { container } = render(
      <BalanceCard balance={{ remaining: 0, monthlySpend: null, currency: 'credits' }} />,
    );
    expect(container.firstChild).toBeNull();
  });
});
