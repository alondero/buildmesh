import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { UsageBar } from '../../src/components/AppSettings/UsageRender';

describe('UsageBar', () => {
  it('renders 100% remaining when 0% is used, not N/A', () => {
    // Regression: Antigravity Claude/GPT-OSS models report 0% used; a `> 0`
    // guard wrongly treated that as missing data and showed "N/A".
    render(<UsageBar window={{ label: 'Claude Sonnet 4.6 (Thinking)', usedPercent: 0, resetsAt: null }} />);
    expect(screen.getByText('100.0% remaining')).toBeTruthy();
    expect(screen.queryByText('N/A')).toBeNull();
  });

  it('renders a known non-zero percentage', () => {
    render(<UsageBar window={{ label: 'Gemini 3.5 Flash (Medium)', usedPercent: 20, resetsAt: null }} />);
    expect(screen.getByText('80.0% remaining')).toBeTruthy();
  });

  it('shows N/A only when usedPercent is genuinely unknown (null)', () => {
    render(<UsageBar window={{ label: 'Unknown', usedPercent: null, resetsAt: null }} />);
    expect(screen.getByText('N/A')).toBeTruthy();
  });
});
