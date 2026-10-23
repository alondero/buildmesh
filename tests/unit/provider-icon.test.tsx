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

  it('renders the Meta infinity-loop mark for the Muse harness and provider ids', () => {
    // The Muse harness (`muse`) and its First-class Model Provider
    // (`muse-code`) share one brand record. Both must take the inline-SVG
    // path (the official Meta mark), never the gray-dot fallback or the
    // old placeholder "M" glyph.
    for (const id of ['muse', 'muse-code']) {
      const { container } = render(<ProviderIcon providerId={id} />);
      expect(container.querySelector('svg')).toBeTruthy();
      expect(container.querySelector('span.bg-text-muted')).toBeNull();
      // The rendered mark must be the Meta infinity loop, not the old
      // placeholder "M" glyph — pin on the path-data fingerprint.
      const d = container.querySelector('svg path')?.getAttribute('d') ?? '';
      expect(d).toContain('M6.915 4.03');
    }
  });

  it('renders the Meta infinity-loop mark for the WSL Muse profile ids (muse-wsl, muse-wsl-<hash>)', () => {
    // WSL profile ids (built by `agent/detection.rs::detect_wsl_profiles`)
    // carry the form `<harness>-wsl[-<distrohash>]` and show up as native
    // Spawn Option rows whose `id == profile.id`. The brand registry
    // must strip the runtime suffix so the Muse WSL row renders the
    // official Meta mark — the same one the native `muse` row uses —
    // not the wire `meta.icon` "M" fallback glyph. Without the suffix
    // strip, the WSL Meta Code row silently degrades to the placeholder
    // the recent #1711 commit replaced for every other Muse surface.
    for (const id of ['muse-wsl', 'muse-wsl-5562756e7475']) {
      const { container } = render(<ProviderIcon providerId={id} />);
      expect(container.querySelector('svg')).toBeTruthy();
      expect(container.querySelector('span.bg-text-muted')).toBeNull();
      const d = container.querySelector('svg path')?.getAttribute('d') ?? '';
      expect(d).toContain('M6.915 4.03');
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

  // ----- Issue #575 / ADR-0016: Spawn Option composite-id lookup -----
  //
  // A Proxied Provider row's id is `<harness>:<provider>` (e.g.
  // `claude:minimax`). The harness half names the executor; the provider
  // half names the brand mark. The brand registry resolves the *provider*
  // half (e.g. `minimax`), so the right logo is rendered.
  //
  // User-reported issue: "For First-class Providers I expect that we would
  // use the logo (it's just showing a circle currently)". Pre-fix, the
  // looking up the composite id directly missed the brand and fell through
  // to the gray-dot fallback.

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

  it('keeps the full composite id in the hover tooltip (not just the lookup key)', () => {
    // The lookup splits on `:` to find the provider's brand, but the
    // hover tooltip should still show the full id so the user can see
    // the composite form.
    render(<ProviderIcon providerId="claude:minimax" />);
    expect(screen.getByTitle('claude:minimax')).toBeTruthy();
  });

  // ----- Issue #328 — `backgroundColor` + `chipSize` props -----
  //
  // Mobile `NodeRow` drives the chip's background from the live
  // `listProviders()` payload (`meta.color`) so the chip can't drift from
  // the row label / status bar. The mobile `ProviderPicker` Proxied-child
  // rows use a 28px chip (smaller than the 34px native row). Both
  // overrides have to compose with the existing brand-mark lookup so the
  // brand PNG/SVG stays in the chip.

  it('backgroundColor overrides the registered brand colour', () => {
    // Anthropic's hard-coded chip is `#1d7cfc` (Claude blue). The
    // override must win — a regression that re-introduces a parallel
    // colour map (the issue #328 motivation) would silently pick the
    // wrong colour when the live payload disagrees with the lookup.
    const { container } = render(
      <ProviderIcon
        providerId="anthropic"
        withBackground
        backgroundColor="#abcdef"
      />,
    );
    const chip = container.firstElementChild as HTMLElement;
    expect(chip.style.background).toBe('rgb(171, 205, 239)');
  });

  it('backgroundColor wins for a Proxied row (live MiniMax colour, not Claude blue)', () => {
    // `claude:minimax` would normally render the MiniMax indigo chip
    // (`#6366f1`) via the lookup. The override must win when the live
    // payload disagrees — a parallel colour-map regression would pick
    // the harness half (`claude` → `#1d7cfc`) instead.
    const { container } = render(
      <ProviderIcon
        providerId="claude:minimax"
        withBackground
        backgroundColor="#abcdef"
      />,
    );
    const chip = container.firstElementChild as HTMLElement;
    expect(chip.style.background).toBe('rgb(171, 205, 239)');
  });

  // ----- Issue #948 — `fallbackGlyph` for custom Proxied accounts -----
  //
  // A custom Claude-compatible Proxied account (`claude:<slug>`) has no
  // registered brand, so it falls through to the gray dot — losing the
  // wire `meta.icon` letter the pre-#328 mobile
  // NodeRow rendered. `fallbackGlyph` lets the caller feed that letter
  // back in without touching the brand-mark path.

  it('renders fallbackGlyph as chip text when the icon lookup misses', () => {
    const { container } = render(
      <ProviderIcon
        providerId="claude:custom-account"
        withBackground
        fallbackGlyph="X"
        className="h-4 w-4"
      />,
    );
    const chip = container.firstElementChild as HTMLElement;
    // Gray chip is preserved (no brand colour for a custom slug)…
    expect(chip.style.background).toBe('rgb(85, 85, 85)'); // #555
    // …but the wire letter shows through instead of the mute dot.
    expect(chip.textContent).toBe('X');
    expect(chip.querySelector('span.bg-text-muted')).toBeNull();
    expect(chip.querySelector('img, svg')).toBeNull();
  });

  it('fallbackGlyph never displaces a brand mark', () => {
    // The glyph is a *fallback*: a provider that resolves to an inline SVG
    // or a brand image must keep it even when the caller passes a glyph
    // (the live payload always carries `meta.icon`, so every mobile row
    // passes one).
    // (`textContent` is no use here — an inline icon's <title> counts
    // towards it. The glyph is the only <span> ProviderIcon can emit
    // alongside a resolved brand mark, so its absence is the assertion.)
    const { container: inline } = render(
      <ProviderIcon providerId="claude:kimi" withBackground fallbackGlyph="K" />,
    );
    expect(inline.querySelector('svg')).toBeTruthy();
    expect(inline.querySelector('span')).toBeNull();

    const { container: image } = render(
      <ProviderIcon providerId="agy" withBackground fallbackGlyph="G" />,
    );
    expect(image.querySelector('img')).toBeTruthy();
    expect(image.querySelector('span')).toBeNull();
  });

  it('falls back to the mute dot when fallbackGlyph is absent or empty', () => {
    // `ProviderInfo.icon` is a non-optional string on the wire, so an
    // adapter that ships `icon: ""` must not render an empty glyph span
    // where the dot used to be — the row's left edge would lose its
    // rhythm entirely.
    for (const glyph of [undefined, '']) {
      const { container } = render(
        <ProviderIcon providerId="mystery" withBackground fallbackGlyph={glyph} />,
      );
      const chip = container.firstElementChild as HTMLElement;
      expect(chip.querySelector('span.bg-text-muted')).toBeTruthy();
      expect(chip.textContent).toBe('');
    }
  });

  it('fallbackGlyph keeps the hover tooltip the dot fallback carried', () => {
    render(
      <ProviderIcon providerId="claude:custom-account" fallbackGlyph="X" title="My Proxy" />,
    );
    expect(screen.getByTitle('My Proxy').textContent).toBe('X');
  });
});
