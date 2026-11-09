/**
 * Type density — the Compact/Comfortable preference (issue #2739).
 *
 * Pins three contracts:
 *
 *   1. `src/lib/typeDensity.ts` — comfortable is the default, the choice
 *      survives a relaunch, and it lands on `<html>` as
 *      `data-type-density="compact"` so the first paint is already at the
 *      user's ladder.
 *   2. `src/App.css` — the role ladder exists, comfortable sets a functional
 *      label at 13px, and the compact variant restates the same ladder one
 *      step down (11.375px) rather than scaling it by an arbitrary factor.
 *   3. The compact variant is a *type* lever only: the root font-size stays
 *      13px, so `rem` — and therefore spacing, control heights and the title
 *      bar's width tiers — keeps its geometry.
 *
 * The CSS is parsed rather than rendered because jsdom applies no
 * custom-property cascade, the same reason the text-scale test parses
 * App.css.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

const STORAGE_KEY = 'buildmesh.type-density';
const HTML_ATTRIBUTE = 'data-type-density';

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute(HTML_ATTRIBUTE);
  // typeDensity.ts reads localStorage AT MODULE LOAD, so each test imports a
  // fresh module after its own setup.
  vi.resetModules();
});

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute(HTML_ATTRIBUTE);
});

describe('typeDensity.ts — default and load', () => {
  it('defaults to comfortable', async () => {
    const { currentTypeDensity, TYPE_DENSITY_DEFAULT } = await import('../../src/lib/typeDensity');
    expect(currentTypeDensity()).toBe(TYPE_DENSITY_DEFAULT);
    expect(TYPE_DENSITY_DEFAULT).toBe('comfortable');
  });

  it('reads a stored compact choice at module load', async () => {
    localStorage.setItem(STORAGE_KEY, 'compact');
    const { currentTypeDensity } = await import('../../src/lib/typeDensity');
    expect(currentTypeDensity()).toBe('compact');
  });

  it('ignores garbage in localStorage', async () => {
    localStorage.setItem(STORAGE_KEY, 'roomy');
    const { currentTypeDensity, TYPE_DENSITY_DEFAULT } = await import('../../src/lib/typeDensity');
    expect(currentTypeDensity()).toBe(TYPE_DENSITY_DEFAULT);
  });
});

describe('typeDensity.ts — writes', () => {
  it('marks <html> and persists compact', async () => {
    const { setTypeDensity } = await import('../../src/lib/typeDensity');
    setTypeDensity('compact');
    expect(document.documentElement.getAttribute(HTML_ATTRIBUTE)).toBe('compact');
    expect(localStorage.getItem(STORAGE_KEY)).toBe('compact');
  });

  it('returning to comfortable removes both the attribute and the stored value', async () => {
    const { setTypeDensity } = await import('../../src/lib/typeDensity');
    setTypeDensity('compact');
    setTypeDensity('comfortable');
    expect(document.documentElement.getAttribute(HTML_ATTRIBUTE)).toBeNull();
    expect(localStorage.getItem(STORAGE_KEY)).toBeNull();
  });

  it('applyTypeDensity() puts a persisted choice on <html> before anything mounts', async () => {
    localStorage.setItem(STORAGE_KEY, 'compact');
    const { applyTypeDensity } = await import('../../src/lib/typeDensity');
    applyTypeDensity();
    expect(document.documentElement.getAttribute(HTML_ATTRIBUTE)).toBe('compact');
  });

  it('notifies subscribers on a change, and not when the value is re-applied', async () => {
    const { onTypeDensityChange, setTypeDensity } = await import('../../src/lib/typeDensity');
    const listener = vi.fn();
    const unsubscribe = onTypeDensityChange(listener);

    setTypeDensity('compact');
    expect(listener).toHaveBeenCalledWith('compact');

    setTypeDensity('compact');
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    setTypeDensity('comfortable');
    expect(listener).toHaveBeenCalledTimes(1);
  });
});

describe('App.css — the type-role ladder', () => {
  it('declares the three roles', () => {
    expect(APP_CSS).toMatch(/--text-label:\s*calc\(/);
    expect(APP_CSS).toMatch(/--text-meta:\s*calc\(/);
    expect(APP_CSS).toMatch(/--text-micro:\s*calc\(/);
  });

  it('sets a comfortable functional label at the 13px interface size', () => {
    // The policy this issue exists to record: a functional label is drawn at
    // the root size, so it is never treated as microcopy.
    expect(APP_CSS).toMatch(/--text-label:\s*calc\(13px\s*\*\s*var\(--ui-text-scale\)\)/);
  });

  it('keeps metadata one step below the label and microcopy at the 10px floor', () => {
    expect(APP_CSS).toMatch(/--text-meta:\s*calc\(11\.375px\s*\*\s*var\(--ui-text-scale\)\)/);
    expect(APP_CSS).toMatch(/--text-micro:\s*calc\(10px\s*\*\s*var\(--ui-text-scale\)\)/);
  });

  it('restates the compact ladder at the pre-#2739 sizes', () => {
    // Compact is not "comfortable times a factor" — it restates the exact
    // label and metadata ladder one step down. (It is a LADDER, not a
    // restoration of the old screens: before the roles existed every surface
    // chose its own size, so the ladder is what is pinned, not the old
    // pixels.)
    expect(APP_CSS).toMatch(
      /:root\[data-type-density='compact'\]\s*\.text-label\s*{[^}]*font-size:\s*calc\(11\.375px/,
    );
    expect(APP_CSS).toMatch(
      /:root\[data-type-density='compact'\]\s*\.text-meta\s*{[^}]*font-size:\s*calc\(9\.75px/,
    );
  });

  it('does not move the root font-size — density is a type lever, not a layout lever', () => {
    expect(APP_CSS).toMatch(/--font-size-base:\s*13px/);
    expect(APP_CSS).toMatch(/html\s*{\s*font-size:\s*var\(--font-size-base\);\s*}/);
  });
});
