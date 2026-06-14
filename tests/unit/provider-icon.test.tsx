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

  it('withBackground wraps known providers in a 34×34 colored chip', () => {
    const { container } = render(
      <ProviderIcon providerId="anthropic" withBackground />,
    );
    const chip = container.firstElementChild as HTMLElement;
    expect(chip.tagName).toBe('DIV');
    expect(chip.style.width).toBe('34px');
    expect(chip.style.height).toBe('34px');
    expect(chip.style.background).toBe('rgb(29, 124, 252)'); // #1d7cfc
    expect(chip.style.color).toBe('rgb(255, 255, 255)');
    // The actual icon is nested inside the chip.
    expect(chip.querySelector('svg')).toBeTruthy();
  });

  it('withBackground uses a neutral chip for unknown providers', () => {
    const { container } = render(
      <ProviderIcon providerId="mystery" withBackground />,
    );
    const chip = container.firstElementChild as HTMLElement;
    expect(chip.style.background).toBe('rgb(85, 85, 85)'); // #555
    // The inner fallback dot is still a span.
    const span = chip.querySelector('span');
    expect(span).toBeTruthy();
    expect(span?.className).toContain('bg-gray-500');
  });

  it('without withBackground, the chip wrapper is NOT rendered (desktop default)', () => {
    const { container } = render(<ProviderIcon providerId="anthropic" />);
    // No 34×34 wrapper — the icon is the root element.
    const root = container.firstElementChild as HTMLElement;
    expect(root.tagName).toBe('svg');
    expect(root.style.width).toBe('');
  });
});
