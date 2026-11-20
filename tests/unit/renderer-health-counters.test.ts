/**
 * The renderer health counters (issue #2833).
 *
 * Two things are pinned here:
 *
 * * the counter registry's own semantics — a count is read **and cleared**
 *   at each sample, and the *count* of a counter is the number of events,
 *   not the number of retained samples;
 * * the commit points that feed it. A counter that is bumped in the wrong
 *   place is worse than no counter, because the line then reports a number
 *   nobody can trust.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import type { Terminal } from '@xterm/xterm';

vi.mock('@xterm/addon-webgl', () => ({
  WebglAddon: class {
    dispose = vi.fn();
    onContextLoss = vi.fn();
  },
}));

import {
  attachedTerminalCount,
  bumpRendererActivity,
  liveWebglRendererCount,
  readAndResetRendererActivity,
  readRendererActivity,
  registerAttachedTerminalCounter,
  registerLiveWebglCounter,
  resetRendererActivity,
} from '../../src/lib/diagnostics/rendererHealthCounters';
import { WebglRendererPool } from '../../src/components/Terminal/WebglRendererPool';

function makeTerm(): Terminal {
  return { loadAddon: vi.fn() } as unknown as Terminal;
}

describe('renderer health counters', () => {
  beforeEach(() => {
    resetRendererActivity();
  });

  it('reads and clears the counters at each sample', () => {
    bumpRendererActivity('view_switch');
    bumpRendererActivity('terminal_attach', 3);

    const first = readAndResetRendererActivity();
    expect(first.view_switch).toBe(1);
    expect(first.terminal_attach).toBe(3);

    // The second window saw nothing: the counters were cleared on read.
    const second = readAndResetRendererActivity();
    expect(second.view_switch).toBe(0);
    expect(second.terminal_attach).toBe(0);
  });

  it('ignores a non-positive bump', () => {
    bumpRendererActivity('dialog', 0);
    bumpRendererActivity('dialog', -1);
    expect(readRendererActivity().dialog).toBe(0);
  });

  it('sums the registered terminal and WebGL gauges', () => {
    registerAttachedTerminalCounter('test-agent', () => 2);
    registerAttachedTerminalCounter('test-buildRun', () => 1);
    registerLiveWebglCounter('test-webgl', () => 3);

    expect(attachedTerminalCount()).toBeGreaterThanOrEqual(3);
    expect(liveWebglRendererCount()).toBeGreaterThanOrEqual(3);
  });

  it('keeps a broken gauge from taking the whole line down', () => {
    registerAttachedTerminalCounter('test-broken', () => {
      throw new Error('gauge exploded');
    });
    expect(() => attachedTerminalCount()).not.toThrow();
  });

  it('counts a WebGL pool eviction', () => {
    const pool = new WebglRendererPool(1);
    pool.activate('a', makeTerm());
    expect(readRendererActivity().webgl_eviction).toBe(0);

    pool.activate('b', makeTerm());
    expect(readRendererActivity().webgl_eviction).toBe(1);

    // A promotion is not an eviction.
    pool.activate('b', makeTerm());
    expect(readRendererActivity().webgl_eviction).toBe(1);
  });
});
