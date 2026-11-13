import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { MeshColorPicker } from '../../src/components/Mesh/MeshColorPicker';
import { MESH_PALETTE } from '../../src/lib/meshColors';

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
    const input = screen.getByLabelText('Custom colour') as HTMLInputElement;
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

  it('shows the indicator for keyboard focus only, so a pointer does not masquerade as one', () => {
    const { input, wrapper } = renderPicker(MESH_PALETTE[0].hex);

    // A pointer press on the swatch focuses the input, but that focus is not
    // a keyboard focus: the indicator must stay off, or a mouse user would
    // see a "keyboard" ring they did not ask for.
    fireEvent.mouseDown(input);
    fireEvent.click(input);
    input.focus();
    expect(document.activeElement).toBe(input);
    expect(wrapper.matches(':focus-within')).toBe(true);
    expect(wrapper.matches(':has(input:focus-visible)')).toBe(false);

    // The keyboard path paints it.
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
    // A custom (non-palette) value is "selected": the swatch wears the white
    // selection ring. The focus indicator is the accent-cyan outline, so the
    // two signals read differently.
    const { wrapper } = renderPicker('#123456');
    const classes = wrapper.className.split(/\s+/);
    expect(classes).toContain('ring-white');
    expect(classes).not.toContain('ring-accent-cyan');
    expect(classes).toContain('has-[:focus-visible]:outline-accent-cyan');
  });
});
