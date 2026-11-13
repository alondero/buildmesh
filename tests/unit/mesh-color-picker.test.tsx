import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { MeshColorPicker } from '../../src/components/Mesh/MeshColorPicker';
import { MESH_PALETTE, swatchInk } from '../../src/lib/meshColors';

describe('MeshColorPicker', () => {
  it('renders a swatch for every palette color plus the custom input', () => {
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={() => {}} />);
    for (const p of MESH_PALETTE) {
      expect(screen.getByLabelText(`Colour ${p.hex}`)).toBeTruthy();
    }
    expect(screen.getByLabelText('Custom colour')).toBeTruthy();
  });

  it('marks the selected palette swatch as pressed', () => {
    render(<MeshColorPicker value={MESH_PALETTE[2].hex} onChange={() => {}} />);
    expect(screen.getByLabelText(`Colour ${MESH_PALETTE[2].hex}`).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByLabelText(`Colour ${MESH_PALETTE[0].hex}`).getAttribute('aria-pressed')).toBe('false');
  });

  it('emits the swatch hex on click', () => {
    const onChange = vi.fn();
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={onChange} />);
    fireEvent.click(screen.getByLabelText(`Colour ${MESH_PALETTE[3].hex}`));
    expect(onChange).toHaveBeenCalledWith(MESH_PALETTE[3].hex);
  });

  it('emits a custom hex from the native color input', () => {
    const onChange = vi.fn();
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={onChange} />);
    fireEvent.input(screen.getByLabelText('Custom colour'), { target: { value: '#123456' } });
    expect(onChange).toHaveBeenCalledWith('#123456');
  });
});

/**
 * Issue #2776 — the custom-colour swatch hid keyboard focus.
 *
 * The picker overlays a fully transparent native `<input type="color">` on
 * the visible swatch. The browser's own focus ring is painted on that
 * transparent input, so it was not visible, and the swatch wrapper had no
 * `focus-within` styling either: a keyboard user tabbing to "Custom colour"
 * had no visible focus at all.
 *
 * The repair puts the indicator on the WRAPPER — the only visible affordance
 * — driven by `:focus-visible`, so it stays keyboard-only. These assertions
 * are behavioural: they check that focus really reaches the input and that
 * the selector the indicator hangs off actually matches the focused DOM.
 */
describe('MeshColorPicker focus visibility (issue #2776)', () => {
  function renderPicker(value: string) {
    render(<MeshColorPicker value={value} onChange={() => {}} />);
    // The custom input's accessible name says "(selected)" when it holds the
    // current colour (issue #2775), so match the name by its prefix.
    const input = screen.getByLabelText(/^Custom colour/) as HTMLInputElement;
    const wrapper = input.closest('label') as HTMLLabelElement;
    return { input, wrapper };
  }

  it('puts the custom colour input in the tab order and paints the indicator on the visible swatch', () => {
    const { input, wrapper } = renderPicker(MESH_PALETTE[0].hex);

    // The native input is transparent, so it has no visible ring to carry —
    // and the swatch wrapper clips anything drawn on a descendant.
    expect(input.className.split(/\s+/)).toContain('opacity-0');
    expect(wrapper.className.split(/\s+/)).toContain('overflow-hidden');

    // Focus really lands on the input…
    fireEvent.keyDown(document, { key: 'Tab' });
    input.focus();
    expect(document.activeElement).toBe(input);

    // …and the selector the indicator hangs off now matches the focused DOM,
    // so the wrapper paints a ring while the keyboard is on it.
    expect(wrapper.matches(':has(input:focus-visible)')).toBe(true);

    // The indicator is the app's focus token: a 2px accent-cyan outline,
    // offset clear of the swatch so it is not read as part of the colour.
    const classes = wrapper.className.split(/\s+/);
    expect(classes).toContain('has-[:focus-visible]:outline-2');
    expect(classes).toContain('has-[:focus-visible]:outline-accent-cyan');
    expect(classes).toContain('has-[:focus-visible]:outline-offset-2');
  });

  it('hangs the indicator off :focus-visible, the signal a pointer press does not raise', () => {
    const { input, wrapper } = renderPicker(MESH_PALETTE[0].hex);

    // jsdom 31 implements `:focus-visible` as an alias of `:focus`: it has no
    // pointer/keyboard heuristic (`hasFocusVisible` in jsdom's
    // `living/css/match/selector.js` just calls `hasFocus`). A pointer press
    // that focuses the swatch therefore matches here, even though a browser
    // keeps the keyboard ring off it. That half of the contract belongs to
    // `tests/e2e/focus-visibility-verify-smoke.spec.ts`, which hovers and tabs
    // the real swatch in both themes.
    fireEvent.mouseDown(input);
    fireEvent.click(input);
    input.focus();
    expect(document.activeElement).toBe(input);
    expect(wrapper.matches(':focus-within')).toBe(true);
    expect(wrapper.matches(':has(input:focus-visible)')).toBe(true);

    // What this environment CAN pin is the selector the indicator hangs off:
    // every focus-keyed token on the wrapper is a `has-[:focus-visible]`
    // variant, so no `focus:` or `focus-within:` decoration exists that a
    // pointer user's focus would paint in a browser.
    const focusTokens = wrapper.className
      .split(/\s+/)
      .filter((token) => /^(?:focus|focus-within|has-\[)/.test(token));
    expect(focusTokens.length).toBeGreaterThan(0);
    for (const token of focusTokens) {
      expect(token.startsWith('has-[:focus-visible]:')).toBe(true);
    }

    // The keyboard path paints it too.
    input.blur();
    fireEvent.keyDown(document, { key: 'Tab' });
    input.focus();
    expect(wrapper.matches(':has(input:focus-visible)')).toBe(true);
  });

  it('keeps the indicator off the transparent input, so the focus is not decorated twice', () => {
    const { input } = renderPicker(MESH_PALETTE[0].hex);
    const decorations = input.className
      .split(/\s+/)
      .filter((token) => /^(focus|focus-visible|has-\[)/.test(token))
      .filter((token) => /(ring|outline)/.test(token));
    expect(decorations).toEqual([]);
  });

  it('paints no focus indicator on hover', () => {
    const { wrapper } = renderPicker(MESH_PALETTE[0].hex);
    const hoverDecorations = wrapper.className
      .split(/\s+/)
      .filter((token) => token.startsWith('hover:'))
      .filter((token) => /(ring|outline)/.test(token));
    expect(hoverDecorations).toEqual([]);
  });

  it('keeps a selected colour distinguishable from a focused colour', () => {
    // A custom (non-palette) value is "selected": the swatch wears the
    // theme-aware selection ring (issue #2775). The focus indicator is the
    // accent-cyan outline, so the two signals read differently.
    const { wrapper } = renderPicker('#123456');
    const classes = wrapper.className.split(/\s+/);
    expect(classes).toContain('ring-text-primary');
    expect(classes).not.toContain('ring-accent-cyan');
    expect(classes).toContain('has-[:focus-visible]:outline-accent-cyan');
  });
});

// ---------------------------------------------------------------------------
// The selection indicator — issue #2775.
//
// The ring used to be `ring-white`, which resolved to white in BOTH themes.
// In the light theme the ring AND its offset were white, so the selected
// swatch dissolved into the dialog it sat in. The indicator is now the
// theme-aware `ring-text-primary` step, and the selection also carries a
// checkmark — a cue that does not depend on the ring's hue at all.
// ---------------------------------------------------------------------------

describe('MeshColorPicker selection indicator (#2775)', () => {
  it('marks the selected swatch with the theme-aware ring, not a white ring', () => {
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={() => {}} />);
    const selected = screen.getByLabelText(`Colour ${MESH_PALETTE[0].hex}`);
    expect(selected.className).toContain('ring-text-primary');
    expect(selected.className).toContain('ring-offset-bg-overlay');
    expect(selected.className).not.toContain('ring-white');
  });

  it('leaves the unselected swatches unmarked', () => {
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={() => {}} />);
    for (const p of MESH_PALETTE.slice(1)) {
      const swatch = screen.getByLabelText(`Colour ${p.hex}`);
      expect(swatch.className).not.toContain('ring-text-primary');
      expect(swatch.textContent).not.toContain('✓');
    }
  });

  it('adds a checkmark to the selected swatch — the non-colour cue', () => {
    render(<MeshColorPicker value={MESH_PALETTE[4].hex} onChange={() => {}} />);
    const selected = screen.getByLabelText(`Colour ${MESH_PALETTE[4].hex}`);
    expect(selected.textContent).toContain('✓');
  });

  it('marks a custom colour as selected, on the swatch and in the accessible name', () => {
    render(<MeshColorPicker value="#123456" onChange={() => {}} />);
    const input = screen.getByLabelText('Custom colour (selected)');
    const label = input.closest('label');
    expect(label?.className).toContain('ring-text-primary');
    expect(label?.textContent).toContain('✓');
    // The palette swatches are all unselected now.
    expect(screen.getByLabelText(`Colour ${MESH_PALETTE[0].hex}`).getAttribute('aria-pressed')).toBe('false');
  });

  it('keeps the custom swatch unmarked while a palette colour is selected', () => {
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={() => {}} />);
    const input = screen.getByLabelText('Custom colour');
    const label = input.closest('label');
    expect(label?.className).not.toContain('ring-text-primary');
    expect(label?.textContent).not.toContain('✓');
  });

  it('does not paint the selection with the keyboard-focus style', () => {
    // Focus and selection are DIFFERENT states: focus is the app-wide
    // `:focus-visible` outline, selection is a ring plus the checkmark. A
    // selected-but-focused swatch therefore shows both, and a
    // focused-but-unselected one shows only the focus outline.
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={() => {}} />);
    const selected = screen.getByLabelText(`Colour ${MESH_PALETTE[0].hex}`);
    expect(selected.className).not.toContain('focus-visible');
  });
});

// ---------------------------------------------------------------------------
// The measurable half of the indicator: the ring and the checkmark ink are
// measured against the surfaces they sit on, in BOTH themes.
// ---------------------------------------------------------------------------

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

function srgbChannelToLinear(c: number): number {
  const cs = c / 255;
  return cs <= 0.03928 ? cs / 12.92 : Math.pow((cs + 0.055) / 1.055, 2.4);
}

function hexToRgb(hex: string): { r: number; g: number; b: number } {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
  if (!m) throw new Error(`Not a 6-digit hex colour: ${hex}`);
  return { r: parseInt(m[1], 16), g: parseInt(m[2], 16), b: parseInt(m[3], 16) };
}

function relativeLuminance({ r, g, b }: { r: number; g: number; b: number }): number {
  return (
    0.2126 * srgbChannelToLinear(r) +
    0.7152 * srgbChannelToLinear(g) +
    0.0722 * srgbChannelToLinear(b)
  );
}

function contrastRatio(a: string, b: string): number {
  const la = relativeLuminance(hexToRgb(a));
  const lb = relativeLuminance(hexToRgb(b));
  const lighter = Math.max(la, lb);
  const darker = Math.min(la, lb);
  return (lighter + 0.05) / (darker + 0.05);
}

function themeTokens(theme: 'dark' | 'light'): Map<string, string> {
  const scope =
    theme === 'dark'
      ? /@theme\s*\{([\s\S]*?)\n\}/.exec(APP_CSS)?.[1]
      : /\[data-theme="light"\]\s*\{([\s\S]*?)\n\}/.exec(APP_CSS)?.[1];
  if (!scope) throw new Error(`Could not read the ${theme} token scope from src/App.css`);
  const tokens = new Map<string, string>();
  for (const match of scope.matchAll(/--color-([a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    tokens.set(match[1], match[2].trim());
  }
  return tokens;
}

describe('the mesh swatch indicator contrast (#2775)', () => {
  it.each(['dark', 'light'] as const)(
    'the selection ring is distinguishable from the dialog it sits on (%s)',
    theme => {
      const tokens = themeTokens(theme);
      const ring = tokens.get('text-primary');
      const dialog = tokens.get('bg-overlay');
      expect(ring).toBeDefined();
      expect(dialog).toBeDefined();
      // WCAG 1.4.11 — a graphical indicator needs 3:1 against its neighbour.
      // The ring's neighbour is the offset gap, which is the dialog surface.
      expect(
        contrastRatio(ring!, dialog!),
        `${theme}: the selection ring (${ring}) on the dialog surface (${dialog})`,
      ).toBeGreaterThanOrEqual(3);
    },
  );

  it('paints the checkmark in an ink that reads on every palette swatch', () => {
    for (const p of MESH_PALETTE) {
      const ratio = contrastRatio(swatchInk(p.hex), p.hex);
      expect(
        ratio,
        `the checkmark on ${p.hex} — ${swatchInk(p.hex)} on ${p.hex} — ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(3);
    }
  });

  it('paints the checkmark in an ink that reads on the extreme custom colours', () => {
    // A custom Mesh colour is anything the user picks, so the extremes of
    // the RGB cube bound the ink choice.
    for (const hex of ['#000000', '#ffffff', '#808080', '#ff0000', '#00ff00', '#0000ff']) {
      const ratio = contrastRatio(swatchInk(hex), hex);
      expect(
        ratio,
        `the checkmark on ${hex} — ${swatchInk(hex)} on ${hex} — ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(3);
    }
  });

  it('picks the ink with the higher contrast for a given swatch', () => {
    expect(swatchInk('#ffffff')).toBe('#0a0a0e');
    expect(swatchInk('#000000')).toBe('#ffffff');
  });

  it('keeps the checkmark at 4.4:1 or better at every swatch luminance', () => {
    // The better of the two inks crosses at a relative luminance of about
    // 0.188, where both readings are 4.41:1 — that is the floor. The grey
    // axis walks every luminance there is, so it walks through the crossing.
    let worst = Number.POSITIVE_INFINITY;
    let worstHex = '';
    for (let value = 0; value <= 255; value += 1) {
      const hex = `#${value.toString(16).padStart(2, '0').repeat(3)}`;
      const ratio = contrastRatio(swatchInk(hex), hex);
      if (ratio < worst) {
        worst = ratio;
        worstHex = hex;
      }
    }
    expect(worst, `the worst ink choice is on ${worstHex}`).toBeGreaterThanOrEqual(4.4);
  });
});
