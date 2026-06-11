/**
 * ProviderIcon renders inline SVGs for known provider ids and falls back to
 * a coloured dot for unknown ones. The new "terminal" provider must take
 * the inline SVG path so the dropdown row reads as a real icon and the
 * sidebar's title-bar dot gets the correct accent colour.
 */
import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import { ProviderIcon } from '../../src/components/Providers/ProviderIcon';

describe('ProviderIcon', () => {
  it('renders an inline SVG for the terminal provider id', () => {
    const { container } = render(<ProviderIcon providerId="terminal" />);
    // The inline-icon path returns an <svg>, vs. the fallback which is a
    // <span> with a background colour. Asserting on the tag prevents a
    // future refactor from quietly dropping the terminal icon into the
    // unknown-provider fallback.
    const svg = container.querySelector('svg');
    expect(svg).toBeTruthy();
  });

  it('still renders an inline SVG for the existing known providers', () => {
    for (const id of ['anthropic', 'codex', 'opencode', 'kimi']) {
      const { container } = render(<ProviderIcon providerId={id} />);
      expect(container.querySelector('svg')).toBeTruthy();
    }
  });

  it('falls back to a coloured dot span for unknown provider ids', () => {
    const { container } = render(<ProviderIcon providerId="mystery" />);
    const span = container.querySelector('span');
    expect(span).toBeTruthy();
    expect(span?.className).toContain('bg-gray-500');
  });
});
