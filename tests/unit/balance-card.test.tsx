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
});
