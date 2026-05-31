import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { UsageBar } from '../../src/components/AppSettings/AppSettingsModal';

describe('UsageBar', () => {
  it('renders 0% used (full quota remaining) as a real figure, not N/A', () => {
    // Regression: Antigravity Claude/GPT-OSS models report 0% used; a `> 0`
    // guard wrongly treated that as missing data and showed "N/A".
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
