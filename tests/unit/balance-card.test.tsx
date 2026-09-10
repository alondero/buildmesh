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
  // against) but live spend against an exhausted balance IS still
  // informative — the "Spent this month" row stays.
  it('hides the "Balance remaining" row when remaining is zero', () => {
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

  it('shows "Balance remaining" when remaining is positive (the useful case)', () => {
    render(<BalanceCard balance={{ remaining: 0.01, monthlySpend: null, currency: 'credits' }} />);
    expect(screen.getByText('Balance remaining')).toBeTruthy();
    expect(screen.getByText('credits 0.01')).toBeTruthy();
  });
});
