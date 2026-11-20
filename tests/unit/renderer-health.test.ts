/**
 * The renderer health sampler (issue #2833).
 *
 * The acceptance criterion for this issue is a unit test with **mocked
 * `PerformanceObserver` entries** that produces the expected aggregated
 * line. The mock below stands in for the browser's observer: it records the
 * entry type it was asked for and lets the test push entries into the
 * callback the sampler registered.
 *
 * The line format is asserted exactly, not field by field: the format IS the
 * contract a handed-back log is read against, so a change to a field name or
 * an order has to be a deliberate edit here rather than a silent drift.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import {
  IPC_TOP_N,
  RENDER_HEALTH_PREFIX,
  RendererHealthSampler,
  formatRendererHealthLine,
  percentile,
  sanitizeFieldValue,
  type RendererHealthGauges,
  type RendererHealthPageGauges,
} from '../../src/lib/diagnostics/rendererHealth';
import {
  bumpRendererActivity,
  readAndResetRendererActivity,
  resetRendererActivity,
} from '../../src/lib/diagnostics/rendererHealthCounters';
import {
  normalizeEventName,
  readAndResetIpcEventCounts,
  recordIpcEvent,
  resetIpcEventCounts,
} from '../../src/lib/tauri/eventListen';

// ---- The mocked PerformanceObserver --------------------------------------

interface FakeEntryList {
  getEntries(): readonly unknown[];
}

class FakePerformanceObserver {
  /** The observers installed for each entry type, in installation order. */
  static byType = new Map<string, FakePerformanceObserver[]>();
  /** Entry types the fake refuses to observe (the browser's "unsupported"). */
  static unsupported = new Set<string>();

  type: string | null = null;

  constructor(private readonly callback: (list: FakeEntryList) => void) {
    FakePerformanceObserver.byType.set('__pending__', FakePerformanceObserver.byType.get('__pending__') ?? []);
  }

  observe(init: { type: string }): void {
    if (FakePerformanceObserver.unsupported.has(init.type)) {
      throw new TypeError(`Unsupported entry type: ${init.type}`);
    }
    this.type = init.type;
    const observers = FakePerformanceObserver.byType.get(init.type) ?? [];
    observers.push(this);
    FakePerformanceObserver.byType.set(init.type, observers);
  }

  disconnect(): void {}

  takeRecords(): readonly unknown[] {
    return [];
  }

  /** Push entries into the sampler, the way the browser would. */
  fire(entries: readonly unknown[]): void {
    this.callback({ getEntries: () => entries });
  }
}

function observersFor(type: string): FakePerformanceObserver[] {
  return FakePerformanceObserver.byType.get(type) ?? [];
}

// ---- Fixtures -------------------------------------------------------------

const FIXED_EPOCH_MS = 1_700_000_000_000;

const FIXED_ANIMATIONS = { running: 12, paused: 3, finished: 0, idle: 0, pending: 0 };

const FIXED_PAGE_GAUGE: RendererHealthPageGauges = {
  timeOriginMs: 1_699_999_990_000,
  domElements: 1234,
  jsHeapBytes: 45 * 1024 * 1024,
  attachedTerminals: 3,
  liveWebglRenderers: 1,
  visibility: 'visible',
  focused: true,
  idle: false,
};

const FIXED_GAUGE: RendererHealthGauges = {
  ...FIXED_PAGE_GAUGE,
  animations: FIXED_ANIMATIONS,
  animationsAgeMs: 0,
};

/**
 * A sampler with every nondeterministic seam pinned: no timer, a fixed
 * clock, a fixed epoch, and a captured line.
 */
function makeSampler(overrides: Partial<ConstructorParameters<typeof RendererHealthSampler>[0]> = {}) {
  const lines: string[] = [];
  const sampler = new RendererHealthSampler({
    emit: line => lines.push(line),
    schedule: () => 0,
    cancel: () => {},
    now: () => 0,
    epochNow: () => FIXED_EPOCH_MS,
    readGauges: () => FIXED_PAGE_GAUGE,
    readAnimationStates: () => FIXED_ANIMATIONS,
    ...overrides,
  });
  return { sampler, lines };
}

// ---- Tests ----------------------------------------------------------------

describe('renderer health sampler (issue #2833)', () => {
  let originalPerformanceObserver: unknown;

  beforeEach(() => {
    FakePerformanceObserver.byType = new Map();
    FakePerformanceObserver.unsupported = new Set();
    resetRendererActivity();
    resetIpcEventCounts();
    originalPerformanceObserver = (globalThis as unknown as { PerformanceObserver?: unknown }).PerformanceObserver;
    (globalThis as unknown as { PerformanceObserver?: unknown }).PerformanceObserver = FakePerformanceObserver;
  });

  afterEach(() => {
    (globalThis as unknown as { PerformanceObserver?: unknown }).PerformanceObserver = originalPerformanceObserver;
  });

  it('aggregates mocked PerformanceObserver entries into the expected line', () => {
    const { sampler, lines } = makeSampler();
    sampler.start();

    // Two keyboard interactions and one pointer interaction.
    observersFor('event')[0].fire([
      { name: 'keydown', startTime: 1000, processingStart: 1010, processingEnd: 1012, duration: 20 },
      { name: 'keydown', startTime: 2000, processingStart: 2030, processingEnd: 2035, duration: 40 },
      { name: 'click', startTime: 3000, processingStart: 3005, processingEnd: 3008, duration: 16 },
    ]);

    // One long frame, with the script that occupied it.
    observersFor('long-animation-frame')[0].fire([
      {
        duration: 210,
        startTime: 500,
        scripts: [
          { sourceURL: 'http://localhost:1420/src/busy.ts', invoker: 'BUTTON#onClick', duration: 180 },
        ],
      },
    ]);

    bumpRendererActivity('view_switch');
    bumpRendererActivity('terminal_attach', 2);

    recordIpcEvent('circuit-run-updated');
    recordIpcEvent('circuit-run-updated');
    recordIpcEvent('circuit-run-updated');
    recordIpcEvent('agent-lifecycle');
    recordIpcEvent('agent-lifecycle');
    recordIpcEvent('node-created');
    recordIpcEvent('git-changed');

    const line = sampler.sample();

    expect(lines).toEqual([line]);
    expect(line).toBe(
      'RENDER_DIAG age_ms=10000 time_origin=1699999990000 vis=visible focus=1 idle=0 dom=1234 '
      + 'anim=running:12/paused:3/finished:0/idle:0/pending:0 anim_age_ms=0 heap_mb=45 term=3 webgl=1 '
      + 'ev_kb=n:2/p50:10/p90:30/max:30/proc_p50:2/proc_p90:5/proc_max:5/pres_p50:5/pres_p90:8/pres_max:8 '
      + 'ev_ptr=n:1/p50:5/p90:5/max:5/proc_p50:3/proc_p90:3/proc_max:3/pres_p50:8/pres_p90:8/pres_max:8 '
      + 'lf=n:1/total_ms:210/max_ms:210 '
      + 'act=view_switch:1/terminal_attach:2/terminal_detach:0/webgl_eviction:0/node_status:0/'
      + 'dialog:0/toast:0/focus_change:0 '
      + 'ipc=circuit-run-updated:3,agent-lifecycle:2,git-changed:1,node-created:1 '
      + 'cost_ms=0 lf_src=http://localhost:1420/src/busy.ts lf_invoker=BUTTON#onClick',
    );
  });

  it('reports only the top five IPC event names', () => {
    const { sampler } = makeSampler();
    // Names that do NOT end in a number: a trailing id is folded onto the
    // stream it belongs to, so `stream-0`..`stream-6` would be one name.
    for (const name of ['stream-a', 'stream-b', 'stream-c', 'stream-d', 'stream-e', 'stream-f', 'stream-g']) {
      recordIpcEvent(name);
    }
    recordIpcEvent('noisy');
    recordIpcEvent('noisy');

    const line = sampler.sample();
    const ipc = line.split(' ').find(field => field.startsWith('ipc='))!;

    expect(ipc.split(',')).toHaveLength(IPC_TOP_N);
    expect(ipc).toContain('noisy:2');
    expect(ipc).not.toContain('stream-e');
  });

  it('falls back to longtask where long-animation-frame is unsupported', () => {
    FakePerformanceObserver.unsupported.add('long-animation-frame');
    const { sampler } = makeSampler();
    sampler.start();

    expect(observersFor('long-animation-frame')).toHaveLength(0);
    expect(observersFor('longtask')).toHaveLength(1);

    observersFor('longtask')[0].fire([
      { name: 'self', duration: 120, attribution: [{ name: 'self', containerType: 'window', containerSrc: 'http://localhost:1420/src/legacy.ts' }] },
    ]);

    const line = sampler.sample();
    expect(line).toContain('lf=n:1/total_ms:120/max_ms:120');
    expect(line).toContain('lf_src=http://localhost:1420/src/legacy.ts');
  });

  it('clears the window counters between two samples', () => {
    const { sampler, lines } = makeSampler();
    sampler.start();

    observersFor('event')[0].fire([
      { name: 'keydown', startTime: 0, processingStart: 5, processingEnd: 6, duration: 10 },
    ]);
    bumpRendererActivity('dialog');

    const first = sampler.sample();
    const second = sampler.sample();

    expect(first).toContain('ev_kb=n:1');
    expect(first).toContain('dialog:1');
    // The second window saw nothing: the counters were read AND cleared.
    expect(second).toContain('ev_kb=n:0');
    expect(second).toContain('dialog:0');
    expect(lines).toHaveLength(2);
  });

  it('takes the animation census on its own cadence and reports its age', () => {
    let now = 0;
    let censuses = 0;
    const { sampler } = makeSampler({
      now: () => now,
      readAnimationStates: () => {
        censuses += 1;
        return FIXED_ANIMATIONS;
      },
      animationCensusIntervalMs: 60_000,
    });

    // The first window takes the census, because there is nothing cached yet.
    const first = sampler.sample();
    expect(censuses).toBe(1);
    expect(first).toContain('anim_age_ms=0');

    // The next window reuses it: the census is not due, and the line says
    // how stale the animation figures are.
    now = 10_000;
    const second = sampler.sample();
    expect(censuses).toBe(1);
    expect(second).toContain('anim_age_ms=10000');

    // A minute later the census is due again.
    now = 60_000;
    const third = sampler.sample();
    expect(censuses).toBe(2);
    expect(third).toContain('anim_age_ms=0');
  });

  it('reports the sampler\'s own cost for the window on the line it writes', () => {
    let tick = 0;
    const { sampler, lines } = makeSampler({
      now: () => (tick++ === 0 ? 0 : 0.5),
    });
    const line = sampler.sample();
    expect(line).toContain('cost_ms=0.5');
    expect(lines[0]).toBe(line);
  });

  it('writes a line when the timer fires', () => {
    let tick: (() => void) | null = null;
    const { sampler, lines } = makeSampler({
      schedule: callback => {
        tick = callback;
        return 1;
      },
    });
    sampler.start();
    expect(lines).toHaveLength(0);

    tick!();
    expect(lines).toHaveLength(1);
    expect(lines[0].startsWith(`${RENDER_HEALTH_PREFIX} `)).toBe(true);
  });

  it('stops writing once stopped', () => {
    let tick: (() => void) | null = null;
    const { sampler, lines } = makeSampler({
      schedule: callback => {
        tick = callback;
        return 1;
      },
    });
    sampler.start();
    sampler.stop();
    tick!();
    expect(lines).toHaveLength(0);
  });
});

describe('renderer health line format', () => {
  it('reports an unavailable JS heap as off rather than as zero', () => {
    const line = formatRendererHealthLine({
      epochMs: 0,
      gauges: { ...FIXED_GAUGE, jsHeapBytes: null },
      keyboard: emptyStats(),
      pointer: emptyStats(),
      longFrames: { count: 0, totalMs: 0, maxMs: 0, topSource: null, topInvoker: null },
      activity: {},
      ipc: [],
      costMs: 0,
    });
    expect(line).toContain('heap_mb=off');
  });

  it('collapses whitespace in a free-text value so a line cannot be split', () => {
    expect(sanitizeFieldValue('http://host/a b\nc')).toBe('http://host/a_b_c');
  });

  it('computes a percentile by nearest rank', () => {
    expect(percentile([], 0.5)).toBe(0);
    expect(percentile([5], 0.9)).toBe(5);
    expect(percentile([1, 2, 3, 4], 0.5)).toBe(2);
    expect(percentile([1, 2, 3, 4], 0.9)).toBe(4);
  });
});

describe('IPC event counting', () => {
  it('folds a per-session event name onto the stream it belongs to', () => {
    expect(normalizeEventName('build-run-output-42')).toBe('build-run-output-<id>');
    expect(normalizeEventName('agent-output')).toBe('agent-output');
  });

  it('orders the counts by volume and clears them on read', () => {
    resetIpcEventCounts();
    recordIpcEvent('a');
    recordIpcEvent('b');
    recordIpcEvent('b');

    const counts = readAndResetIpcEventCounts();
    expect(counts[0]).toEqual({ name: 'b', count: 2 });
    expect(counts[1]).toEqual({ name: 'a', count: 1 });
    expect(readAndResetIpcEventCounts()).toEqual([]);
  });
});

function emptyStats() {
  return {
    count: 0,
    p50: 0,
    p90: 0,
    max: 0,
    processingP50: 0,
    processingP90: 0,
    processingMax: 0,
    presentationP50: 0,
    presentationP90: 0,
    presentationMax: 0,
  };
}
