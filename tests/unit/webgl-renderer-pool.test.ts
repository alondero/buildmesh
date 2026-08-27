/**
 * GPU context-loss crash mitigation (2026-08-26 diagnosis): the pool caps
 * live xterm WebGL contexts at a small LRU window so 15+ terminals can't
 * blow past Chromium's ~16-context budget and churn GPU context
 * creation/eviction ahead of NVIDIA driver resets.
 *
 * `@xterm/addon-webgl` is mocked at module level (same pattern as
 * load-webgl-renderer.test.ts); each `new WebglAddon()` returns a fresh
 * mock with spy `dispose`/`onContextLoss`, captured in `addons` so tests
 * can assert eviction and context-loss behaviour.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { Terminal } from '@xterm/xterm';

interface MockWebglAddon {
  dispose: ReturnType<typeof vi.fn>;
  onContextLoss: ReturnType<typeof vi.fn>;
}

const addons: MockWebglAddon[] = [];

vi.mock('@xterm/addon-webgl', () => ({
  WebglAddon: class {
    dispose = vi.fn();
    onContextLoss = vi.fn();
    constructor() {
      const addon: MockWebglAddon = { dispose: this.dispose, onContextLoss: this.onContextLoss };
      addons.push(addon);
      return addon;
    }
  },
}));

import { WebglRendererPool } from '../../src/components/Terminal/WebglRendererPool';

function makeTerm(): Terminal {
  return { loadAddon: vi.fn() } as unknown as Terminal;
}

describe('WebglRendererPool', () => {
  beforeEach(() => {
    addons.length = 0;
  });

  it('activates a renderer for a new key', () => {
    const pool = new WebglRendererPool(2);
    pool.activate('a', makeTerm());
    expect(pool.size()).toBe(1);
    expect(addons).toHaveLength(1);
  });

  it('re-activating an existing key promotes it without creating a new addon', () => {
    const pool = new WebglRendererPool(2);
    const term = makeTerm();
    pool.activate('a', term);
    pool.activate('b', makeTerm());
    addons.length = 0; // forget creation-time mocks; count only new ones
    pool.activate('a', term);
    expect(pool.size()).toBe(2);
    expect(addons).toHaveLength(0);
  });

  it('evicts the least-recently-used entry when over budget', () => {
    const pool = new WebglRendererPool(2);
    pool.activate('a', makeTerm());
    pool.activate('b', makeTerm());
    // 'a' is now LRU (activate order: a, b)
    const evicted = addons[0]!;
    pool.activate('c', makeTerm());
    expect(pool.size()).toBe(2);
    expect(evicted.dispose).toHaveBeenCalledTimes(1);
    expect(pool.has('a')).toBe(false);
    expect(pool.has('b')).toBe(true);
    expect(pool.has('c')).toBe(true);
  });

  it('promotion protects an entry from eviction', () => {
    const pool = new WebglRendererPool(2);
    pool.activate('a', makeTerm());
    pool.activate('b', makeTerm());
    // Touch 'a' so 'b' becomes the LRU victim.
    pool.activate('a', makeTerm());
    const bAddon = addons[1]!;
    pool.activate('c', makeTerm());
    expect(pool.has('a')).toBe(true);
    expect(bAddon.dispose).toHaveBeenCalledTimes(1);
  });

  it('release disposes the addon and frees the slot', () => {
    const pool = new WebglRendererPool(2);
    pool.activate('a', makeTerm());
    const addon = addons[0]!;
    pool.release('a');
    expect(addon.dispose).toHaveBeenCalledTimes(1);
    expect(pool.size()).toBe(0);
    // Slot is free — next activate does not evict anything.
    pool.activate('b', makeTerm());
    expect(pool.size()).toBe(1);
  });

  it('re-activating after release creates a fresh addon on the same term', () => {
    const pool = new WebglRendererPool(2);
    const term = makeTerm();
    pool.activate('a', term);
    pool.release('a');
    pool.activate('a', term);
    expect(pool.size()).toBe(1);
    expect(addons).toHaveLength(2);
    expect(term.loadAddon).toHaveBeenCalledTimes(2);
  });

  it('releasing an unknown key is a no-op', () => {
    const pool = new WebglRendererPool(2);
    expect(() => pool.release('nope')).not.toThrow();
    expect(pool.size()).toBe(0);
  });

  it('context loss then eviction does not double-dispose', () => {
    const pool = new WebglRendererPool(1);
    pool.activate('a', makeTerm());
    const addon = addons[0]!;
    // Simulate the driver-reset path: loadWebglRenderer's registered
    // onContextLoss callback fires, disposing the addon internally.
    addon.onContextLoss.mock.calls[0]?.[0]();
    expect(addon.dispose).toHaveBeenCalledTimes(1);
    // Eviction calls the disposer again — must not reach the addon twice.
    pool.activate('b', makeTerm());
    expect(addon.dispose).toHaveBeenCalledTimes(1);
  });

  it('releaseAll disposes every entry', () => {
    const pool = new WebglRendererPool(3);
    pool.activate('a', makeTerm());
    pool.activate('b', makeTerm());
    pool.activate('c', makeTerm());
    pool.releaseAll();
    expect(pool.size()).toBe(0);
    expect(addons.every(a => a.dispose.mock.calls.length === 1)).toBe(true);
  });
});
