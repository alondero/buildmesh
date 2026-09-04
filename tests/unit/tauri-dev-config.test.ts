/**
 * tauri.dev.conf.json overlay drift guard.
 *
 * `tauri build --config src-tauri/tauri.dev.conf.json` merges the overlay
 * into the base `tauri.conf.json` — but Tauri REPLACES arrays wholesale
 * instead of merging them. `app.windows` is an array, so any window field
 * the overlay omits silently reverts to the Tauri default in the dev
 * profile. That's how the dev build lost `decorations: false` and grew a
 * native Windows title bar above the bespoke TitleBar component (plus
 * default 800×600 size and lost min-size/centering) while the stable hub
 * kept its frameless window.
 *
 * This test pins the dev window to the base window: every base window
 * field except `title` must be present with the same value, and the
 * profile-specific identity fields stay distinct. If you add a field to
 * the base window config, this test fails until the overlay copies it —
 * which is exactly the reminder the merge semantics won't give you.
 */
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const base = JSON.parse(
  readFileSync(resolve(process.cwd(), 'src-tauri/tauri.conf.json'), 'utf8'),
) as { app: { windows: Array<Record<string, unknown>> } };

const dev = JSON.parse(
  readFileSync(resolve(process.cwd(), 'src-tauri/tauri.dev.conf.json'), 'utf8'),
) as {
  productName: string;
  identifier: string;
  mainBinaryName: string;
  app: { windows: Array<Record<string, unknown>> };
};

describe('tauri.dev.conf.json overlay (windows array is replaced, not merged)', () => {
  it('redeclares every base window field except the title', () => {
    expect(base.app.windows).toHaveLength(1);
    expect(dev.app.windows).toHaveLength(1);
    const [baseWindow] = base.app.windows;
    const [devWindow] = dev.app.windows;
    for (const [key, value] of Object.entries(baseWindow)) {
      if (key === 'title') continue;
      expect(devWindow, `dev overlay drops base window field "${key}"`).toStrictEqual(
        expect.objectContaining({ [key]: value }),
      );
    }
  });

  it('keeps the dev profile identity distinct', () => {
    expect(dev.productName).toBe('Buildmesh Dev');
    expect(dev.identifier).toBe('com.alond.buildmesh.dev');
    expect(dev.mainBinaryName).toBe('buildmesh-dev');
    expect(dev.app.windows[0].title).toBe('Buildmesh Dev');
  });

  it('keeps the dev window frameless so only the bespoke TitleBar renders', () => {
    expect(dev.app.windows[0].decorations).toBe(false);
  });
});
