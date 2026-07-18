/**
 * ProviderIcon renders inline SVGs for known provider ids and falls back to
 * a coloured dot for unknown ones. The new "terminal" provider must take
 * the inline SVG path so the dropdown row reads as a real icon and the
 * sidebar's title-bar dot gets the correct accent colour.
 */
import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
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

  it('renders the Claude Code mark for the detected "claude" profile id (#534)', () => {
    // Startup auto-detection (detection.rs) registers Claude Code with id
    // "claude", but the icon map historically only keyed the Claude mark under
    // "anthropic" — so a Claude Code row, and any node spawned with it, fell
    // through to the gray-dot fallback instead of showing the logo.
    const { container } = render(<ProviderIcon providerId="claude" />);
    expect(container.querySelector('svg')).toBeTruthy();
    // Not the neutral fallback dot.
    expect(container.querySelector('span')).toBeNull();
  });

  it('falls back to a coloured dot span for unknown provider ids', () => {
    const { container } = render(<ProviderIcon providerId="mystery" />);
    const span = container.querySelector('span');
    expect(span).toBeTruthy();
    expect(span?.className).toContain('bg-text-muted');
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
    expect(span?.className).toContain('bg-text-muted');
  });

  it('without withBackground, the chip wrapper is NOT rendered (desktop default)', () => {
    const { container } = render(<ProviderIcon providerId="anthropic" />);
    // No 34×34 wrapper — the icon is the root element.
    const root = container.firstElementChild as HTMLElement;
    expect(root.tagName).toBe('svg');
    expect(root.style.width).toBe('');
  });

  // ----- Issue #575 / ADR-0016: Spawn Option composite-id lookup -----
  //
  // A Proxied Provider row's id is `<harness>:<provider>` (e.g.
  // `claude:minimax`). The harness half names the executor; the provider
  // half names the brand mark. `INLINE_ICONS` and `COLORED_IMAGES` are
  // keyed on the *provider* (e.g. `minimax` → `minimaxLogo`), so the
  // lookup must split on `:` first to render the right logo.
  //
  // User-reported issue: "For First-class Providers I expect that we would
  // use the logo (it's just showing a circle currently)". Pre-fix, the
  // `INLINE_ICONS[providerId]` lookup at the composite id missed every
  // map entry and fell through to the gray-dot fallback.

  it('renders the MiniMax brand image for a Proxied row with id "claude:minimax"', () => {
    const { container } = render(
      <ProviderIcon providerId="claude:minimax" className="h-5 w-5" />,
    );
    // MiniMax is a brand-coloured <img> with the minimax.svg src; it
    // must NOT be the gray-dot fallback <span>.
    const img = container.querySelector('img');
    expect(img).toBeTruthy();
    expect(container.querySelector('span.bg-text-muted')).toBeNull();
  });

  it('renders the Kimi inline SVG for a Proxied row with id "claude:kimi"', () => {
    const { container } = render(
      <ProviderIcon providerId="claude:kimi" className="h-5 w-5" />,
    );
    // Kimi is an inline SVG.
    expect(container.querySelector('svg')).toBeTruthy();
    expect(container.querySelector('span.bg-text-muted')).toBeNull();
  });

  it('uses the brand-color chip background for a Proxied row (MiniMax indigo, not Claude blue)', () => {
    // Pre-fix: chip background was keyed on `claude:minimax` (a miss,
    // falling through to the dark-gray #555 fallback). Post-fix: keyed
    // on the provider half `minimax` → #6366f1 (indigo).
    const { container } = render(
      <ProviderIcon providerId="claude:minimax" withBackground />,
    );
    const chip = container.firstElementChild as HTMLElement;
    expect(chip.style.background).toBe('rgb(99, 102, 241)');
  });

  it('keeps the full composite id in the hover tooltip (not just the lookup key)', () => {
    // The lookup splits on `:` to find the provider's brand, but the
    // hover tooltip should still show the full id so the user can see
    // the composite form.
    render(<ProviderIcon providerId="claude:minimax" />);
    expect(screen.getByTitle('claude:minimax')).toBeTruthy();
  });

  it('harness_order list filter rejects Proxied rows (#575 user fix)', async () => {
    // The HarnessOrderList in Settings is for reordering Agent Harnesses,
    // NOT Proxied Provider rows. Filter `!p.is_proxied && p.id !== 'terminal'`
    // ensures MiniMax / Kimi don't show up as orderable harnesses.
    const { HarnessOrderList, reorderIds } = await import(
      '../../src/components/AppSettings/HarnessOrderList'
    );
    const { container } = render(
      <HarnessOrderList
        providers={[
          { id: 'claude', label: 'Claude Code', color: '', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
          { id: 'claude:minimax', label: 'MiniMax', color: '', icon: '', resumable: true, harness_id: 'claude', provider_id: 'minimax', is_proxied: true, group_key: 'claude' },
          { id: 'claude:kimi', label: 'Kimi', color: '', icon: '', resumable: true, harness_id: 'claude', provider_id: 'kimi', is_proxied: true, group_key: 'claude' },
          { id: 'codex', label: 'Codex', color: '', icon: '', resumable: false, harness_id: 'codex', provider_id: null, is_proxied: false, group_key: 'codex' },
          { id: 'terminal', label: 'Terminal', color: '', icon: '', resumable: false, harness_id: 'terminal', provider_id: null, is_proxied: false, group_key: 'terminal' },
        ]}
        onReorder={() => {}}
      />,
    );
    // Orderable rows: claude + codex (terminal is pinned, proxied rows
    // are not harnesses). Both must have a drag handle.
    const dragHandles = container.querySelectorAll('[aria-label^="Reorder "]');
    const labels = Array.from(dragHandles).map((h) => h.getAttribute('aria-label'));
    expect(labels).toEqual(['Reorder Claude Code', 'Reorder Codex']);
    expect(labels).not.toContain('Reorder MiniMax');
    expect(labels).not.toContain('Reorder Kimi');
    expect(labels).not.toContain('Reorder Terminal');
    // Sanity: the pure reorder math still composes the order array
    // from only the orderable rows.
    expect(reorderIds(['claude', 'codex'], 'codex', 'claude')).toEqual(['codex', 'claude']);
  });
});
