/**
 * Interface text scale — the accessibility size lever.
 *
 * Pins two contracts:
 *
 *   1. `src/lib/textScale.ts` — the default is the size the app ships with,
 *      the selectable range is enforced, the value survives a relaunch, and
 *      it lands on `<html>` as the `--ui-text-scale` custom property so the
 *      first paint is already at the user's size.
 *   2. `src/App.css` — every type step actually multiplies by that property.
 *      A step that drops the multiplier would silently stay at its shipped
 *      size while the rest of the app grows, which is the failure this
 *      setting exists to avoid; the CSS is parsed rather than rendered
 *      because jsdom applies no custom-property cascade.
 *
 * The 13px root staying put is part of the contract, not an implementation
 * detail: `rem` is what every spacing utility, control height and title-bar
 * width tier is measured in (docs/development/windows.md), so scaling the root
 * would move the layout instead of the text.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

const CSS_VAR = '--ui-text-scale';
const STORAGE_KEY = 'buildmesh.text-scale';

beforeEach(() => {
  localStorage.clear();
  document.documentElement.style.removeProperty(CSS_VAR);
  // textScale.ts reads localStorage AT MODULE LOAD, so each test imports a
  // fresh module after its own setup.
  vi.resetModules();
});

afterEach(() => {
  localStorage.clear();
  document.documentElement.style.removeProperty(CSS_VAR);
});

describe('textScale.ts — default and load', () => {
  it('defaults to the shipped size when nothing is stored', async () => {
    const { currentTextScale, TEXT_SCALE_DEFAULT } = await import('../../src/lib/textScale');
    expect(currentTextScale()).toBe(TEXT_SCALE_DEFAULT);
    expect(TEXT_SCALE_DEFAULT).toBe(1);
    // The default leaves no custom property behind — the @theme default (1)
    // is what renders.
    expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('');
  });

  it('reads a stored percentage at module load', async () => {
    localStorage.setItem(STORAGE_KEY, '125');
    const { currentTextScale } = await import('../../src/lib/textScale');
    expect(currentTextScale()).toBe(1.25);
  });

  it('ignores a stored value above the selectable range', async () => {
    localStorage.setItem(STORAGE_KEY, '400');
    const { currentTextScale, TEXT_SCALE_MAX } = await import('../../src/lib/textScale');
    expect(currentTextScale()).toBe(TEXT_SCALE_MAX);
  });

  it('ignores a stored value below the floor (never shrinks past the shipped size)', async () => {
    localStorage.setItem(STORAGE_KEY, '60');
    const { currentTextScale, TEXT_SCALE_MIN } = await import('../../src/lib/textScale');
    expect(currentTextScale()).toBe(TEXT_SCALE_MIN);
  });

  it('ignores garbage in localStorage', async () => {
    localStorage.setItem(STORAGE_KEY, 'large');
    const { currentTextScale, TEXT_SCALE_DEFAULT } = await import('../../src/lib/textScale');
    expect(currentTextScale()).toBe(TEXT_SCALE_DEFAULT);
  });
});

describe('textScale.ts — writes', () => {
  it('writes the scale to <html> and persists the whole percent', async () => {
    const { setTextScale } = await import('../../src/lib/textScale');
    setTextScale(1.25);
    expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('1.25');
    expect(localStorage.getItem(STORAGE_KEY)).toBe('125');
  });

  it('returning to the default removes both the property and the stored value', async () => {
    const { setTextScale, TEXT_SCALE_DEFAULT } = await import('../../src/lib/textScale');
    setTextScale(1.3);
    setTextScale(TEXT_SCALE_DEFAULT);
    expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('');
    expect(localStorage.getItem(STORAGE_KEY)).toBeNull();
  });

  it('clamps to the selectable range', async () => {
    const { applyTextScale, setTextScale, currentTextScale, TEXT_SCALE_MAX, TEXT_SCALE_MIN } =
      await import('../../src/lib/textScale');
    setTextScale(3);
    expect(currentTextScale()).toBe(TEXT_SCALE_MAX);
    applyTextScale();
    expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('1.5');

    setTextScale(0.2);
    expect(currentTextScale()).toBe(TEXT_SCALE_MIN);
    expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('');
  });

  it('quantises to whole percent so the readout and the slider agree', async () => {
    const { setTextScale, currentTextScale } = await import('../../src/lib/textScale');
    setTextScale(1.234);
    expect(currentTextScale()).toBe(1.23);
  });

  it('applyTextScale() puts a persisted value on <html> before anything mounts', async () => {
    localStorage.setItem(STORAGE_KEY, '140');
    const { applyTextScale } = await import('../../src/lib/textScale');
    applyTextScale();
    expect(document.documentElement.style.getPropertyValue(CSS_VAR)).toBe('1.4');
  });

  it('notifies subscribers on a change, and not when the value is re-applied', async () => {
    const { onTextScaleChange, setTextScale } = await import('../../src/lib/textScale');
    const listener = vi.fn();
    const unsubscribe = onTextScaleChange(listener);

    setTextScale(1.15);
    expect(listener).toHaveBeenCalledWith(1.15);

    setTextScale(1.15);
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    setTextScale(1.2);
    expect(listener).toHaveBeenCalledTimes(1);
  });
});

describe('App.css — the type scale follows --ui-text-scale', () => {
  it('declares the scale default in the @theme block', () => {
    expect(APP_CSS).toMatch(/--ui-text-scale:\s*1;/);
  });

  it.each(['2xs', 'xs', 'sm', 'base', 'lg', 'xl', '2xl'])(
    'multiplies the %s step by the scale',
    (step) => {
      const declaration = new RegExp(`--text-${step}:\\s*calc\\([^;]*var\\(--ui-text-scale\\)`);
      expect(APP_CSS).toMatch(declaration);
    },
  );

  it('keeps the root font-size — and therefore rem — at the 13px base', () => {
    // The rem anchor: an html font-size that multiplied by the scale would
    // move spacing, hit areas and the title bar's width tiers with it.
    expect(APP_CSS).toMatch(/--font-size-base:\s*13px/);
    expect(APP_CSS).toMatch(/html\s*{\s*font-size:\s*var\(--font-size-base\);\s*}/);
  });

  it('scales inherited body text, which no text-* utility covers', () => {
    expect(APP_CSS).toMatch(
      /body\s*{\s*font-size:\s*calc\(var\(--font-size-base\)\s*\*\s*var\(--ui-text-scale\)\);\s*}/,
    );
  });
});
