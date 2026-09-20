import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { Wordmark } from '../../src/components/TitleBar/Wordmark';

/**
 * Wordmark theme regression.
 *
 * The title bar sits on `bg-bg-surface`, which is `#ffffff` in the light
 * theme. The wordmark shipped as a raster with near-white text baked into the
 * pixels, so it vanished whenever the user picked light — the artwork had no
 * way to follow `[data-theme]`.
 *
 * These tests pin the property that makes that class of bug impossible rather
 * than the mechanism that happens to deliver it today: the wordmark's text is
 * filled from tokens that BOTH theme blocks declare. Re-introducing a literal
 * colour fails here. Because the binding is pure CSS, a theme flip needs no
 * re-render and there is no subscription to clean up.
 */

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

/** Every value the stylesheet declares for `--<token>`, in source order. */
function declaredValues(token: string): string[] {
  const re = new RegExp(`--${token}:\\s*([^;]+);`, 'g');
  return [...APP_CSS.matchAll(re)].map((match) => match[1].trim());
}

function wordmark() {
  const utils = render(<Wordmark />);
  return { ...utils, root: screen.getByRole('img', { name: 'Buildmesh' }) };
}

describe('TitleBar wordmark', () => {
  it('exposes the brand name to assistive tech', () => {
    const { root } = wordmark();
    // Inline SVG has no `alt`; the accessible name comes from aria-label on
    // the wrapper, which is the correct pattern for an SVG lockup.
    expect(root.getAttribute('aria-label')).toBe('Buildmesh');
    expect(root.tagName).toBe('SPAN');
  });

  describe('theme binding', () => {
    it.each(['color-text-primary', 'color-accent-cyan'])(
      'fills from --%s, which both theme blocks declare',
      (token) => {
        const values = declaredValues(token);
        // Declared more than once, with differing values: i.e. it actually
        // flips between the dark palette and [data-theme="light"].
        expect(values.length).toBeGreaterThanOrEqual(2);
        expect(new Set(values).size).toBeGreaterThanOrEqual(2);
      }
    );

    it('binds each half of the wordmark to its token utility', () => {
      wordmark();
      expect(screen.getByText('build').className).toContain('text-text-primary');
      expect(screen.getByText('mesh').className).toContain('text-accent-cyan');
    });

    it('bakes no literal colour into the wordmark text', () => {
      wordmark();
      for (const part of ['build', 'mesh']) {
        const el = screen.getByText(part);
        expect(el.getAttribute('style')).toBeNull();
        // A hex/rgb utility or an inline fill is exactly the regression the
        // raster wordmark was: a colour that cannot follow [data-theme].
        expect(el.className).not.toMatch(/#[0-9a-f]{3,8}\b|rgba?\(/i);
      }
    });
  });

  describe('drag region', () => {
    it('keeps the wrapper as the element under the pointer', () => {
      const { root } = wordmark();
      // Tauri's drag script only checks the element the pointer is over
      // (`e.target.hasAttribute('data-tauri-drag-region')`). Both children are
      // pointer-events-none so the painted SVG nodes and the text span are
      // never that element — otherwise the bar silently stops dragging over
      // the logo.
      expect(root.hasAttribute('data-tauri-drag-region')).toBe(true);
      const children = Array.from(root.children);
      expect(children).toHaveLength(2);
      for (const child of children) {
        // getAttribute, not .className: SVG elements expose className as an
        // SVGAnimatedString, which stringifies to "[object SVGAnimatedString]".
        expect(child.getAttribute('class') ?? '').toContain('pointer-events-none');
      }
    });
  });

  describe('mark', () => {
    it('keeps the badge plate a fixed dark, independent of the chrome theme', () => {
      const { root } = wordmark();
      const plate = root.querySelector('svg rect');
      // Deliberate, not an oversight: the plate is what keeps the neon wires
      // legible on a white title bar, so it must NOT follow the theme.
      expect(plate?.getAttribute('fill')).toBe('#16161d');
    });
  });
});

/**
 * One canonical compact mark, not three agreeing copies.
 *
 * `src/assets/logo.svg` and `mobile/public/favicon.svg` are generated from
 * `docs/brand/b3-relay-icon.svg` by `npm run brand:build`. If someone edits a
 * copy by hand — the way the logo previously drifted into three disagreeing
 * files — this fails rather than letting the next regeneration silently
 * discard the edit.
 */
describe('compact mark copies', () => {
  const canonical = readFileSync(
    resolve(__dirname, '../../docs/brand/b3-relay-icon.svg'),
    'utf8'
  );

  it.each(['src/assets/logo.svg', 'mobile/public/favicon.svg'])(
    '%s is an unmodified copy of the canonical source',
    (relPath) => {
      const generated = readFileSync(resolve(__dirname, '../..', relPath), 'utf8');
      const [banner, ...body] = generated.split('\n');
      expect(banner).toContain('do not edit by hand');
      expect(banner).toContain('b3-relay-icon.svg');
      // Byte-identical below the banner.
      expect(body.join('\n')).toBe(canonical);
    }
  );
});
