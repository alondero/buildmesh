/**
 * The renderer health line (issue #2833).
 *
 * ## Why
 *
 * Buildmesh has an always-on resource timeline for the **native** process
 * (`logs/diagnostics.log`, `src-tauri/src/diagnostics/mod.rs`) and none for
 * the **WebView**, where the UI actually runs. In issue #2830 typing lagged
 * for days — a keystroke delay median around 65 ms and frame gaps up to
 * 183 ms — and nothing recorded it. Diagnosing it took hand-run console
 * probes, and the reload that fixed it destroyed the evidence.
 *
 * This module is the missing half: a passive sampler that writes one
 * structured line every 10 seconds describing what the renderer was doing.
 *
 * ## What "passive" means here
 *
 * There is **no `requestAnimationFrame` loop** and no per-frame work. The
 * sampler installs `PerformanceObserver`s and lets the browser push entries
 * into it; the only timer is a single `setTimeout` that fires once per
 * window to write the line and re-arm itself. Nothing here polls the app's
 * state on a frame budget.
 *
 * The one unavoidable main-thread cost is reading the page's own size — the
 * DOM element count, the animation list and the JS heap — because those
 * facts only exist on the main thread. That read happens once per 10 s
 * window, and the line reports what it cost (`cost_ms`) so the budget can
 * be checked from the log itself rather than taken on trust.
 *
 * ## The line format
 *
 * One line, `key=value` fields, a stable `RENDER_DIAG` prefix so a
 * handed-back log can be scanned with plain `grep`. The native `DIAG` line
 * uses `label:value` pairs separated by `/` inside a field; the renderer
 * line follows the same convention for its numeric blocks
 * (`anim=running:12/paused:3`). Two exceptions, both because the value is
 * free text rather than a number:
 *
 * * `ipc=` separates its event names with `,` — event names may contain `/`
 *   (`companion://step`) and `:`.
 * * `lf_src=` and `lf_invoker=` are top-level fields, because a script URL
 *   contains `/` and would otherwise be indistinguishable from the field
 *   separator.
 *
 * Free-text values are whitespace-stripped and length-capped so a field can
 * never split the line in two.
 *
 * ## Where the line goes
 *
 * Through the existing frontend log bridge (`src/lib/frontendLog.ts` →
 * `log_frontend`). The native side recognises the `RENDER_DIAG` prefix and
 * writes the line into its own bounded, rotating
 * `logs/renderer-diagnostics.log` — a sibling of `logs/diagnostics.log` —
 * so the renderer timeline can be handed back on its own and never
 * competes with the chatty main log for the user's attention.
 */

import { logFrontend } from '../frontendLog';
import { IDLE_ROOT_ATTRIBUTE } from '../idleAnimationGate';
import { readAndResetIpcEventCounts } from '../tauri/eventListen';
import { isTauriRuntime } from '../tauri/webview';
import { watchWindowFocus } from '../windowFocus';
import {
  attachedTerminalCount,
  bumpRendererActivity,
  liveWebglRendererCount,
  readAndResetRendererActivity,
} from './rendererHealthCounters';

/**
 * The prefix the native side routes on. It must match
 * `RENDER_HEALTH_PREFIX` in `src-tauri/src/diagnostics/renderer.rs`.
 */
export const RENDER_HEALTH_PREFIX = 'RENDER_DIAG';

/** How often a line is written. */
export const DEFAULT_HEALTH_INTERVAL_MS = 10_000;

/**
 * How often the animation census is taken.
 *
 * `document.getAnimations()` is the most expensive thing the sampler does: a
 * Chrome trace of the sampler ticking on its ten-second cadence measured it
 * at roughly 0.5 ms, against 0.02 ms for the DOM element count and 0.04 ms
 * for the heap reading. Reading it every window would put the sampler over
 * the 0.5 ms budget the issue sets, so the census runs on its own slower
 * cadence and the line reports how old it is.
 *
 * Sixty seconds is six windows: stale enough to cost 0.08 ms per window
 * amortised, fresh enough that a run of animations shows up within a minute.
 */
export const DEFAULT_ANIMATION_CENSUS_INTERVAL_MS = 60_000;

/**
 * The shortest event-timing duration the observer reports.
 *
 * The Event Timing spec clamps `durationThreshold` to 16 ms for the `event`
 * type, and Chromium's default is 104 ms — far too coarse to see typing lag,
 * which is the thing this line exists to catch.
 */
const EVENT_TIMING_THRESHOLD_MS = 16;

/**
 * How many samples of each kind are retained for the percentile calculation.
 *
 * The *count* reported is always the true number of entries observed; only
 * the percentile maths is over a bounded window. A window with more than
 * this many events is a pathology the line already reports through its
 * count, and an unbounded array would make the sampler's own cost unbounded
 * too.
 */
const MAX_RETAINED_SAMPLES = 4096;

/** The `playState` buckets the animation gauge reports. */
const ANIMATION_STATES = ['running', 'paused', 'finished', 'idle', 'pending'] as const;

/** Event Timing entry names that are keyboard interactions. */
const KEYBOARD_EVENT_NAMES = new Set(['keydown', 'keyup', 'keypress', 'input']);

/** Event Timing entry names that are pointer interactions. */
const POINTER_EVENT_NAMES = new Set([
  'pointerdown',
  'pointerup',
  'mousedown',
  'mouseup',
  'click',
  'dblclick',
  'auxclick',
  'contextmenu',
  'wheel',
]);

// ---- Types ---------------------------------------------------------------

/** Percentiles and extremes of one interaction kind over a sample window. */
export interface InteractionStats {
  /** How many entries were observed (the true count, not the retained one). */
  count: number;
  /** Input delay — `processingStart - startTime`. */
  p50: number;
  p90: number;
  max: number;
  /** Processing time — `processingEnd - processingStart`. */
  processingP50: number;
  processingP90: number;
  processingMax: number;
  /** Presentation delay — `startTime + duration - processingEnd`. */
  presentationP50: number;
  presentationP90: number;
  presentationMax: number;
}

/** Long-frame statistics over a sample window. */
export interface LongFrameStats {
  /** How many long frames were observed. */
  count: number;
  /** Their summed duration in milliseconds. */
  totalMs: number;
  /** The longest single frame in milliseconds. */
  maxMs: number;
  /** The `sourceURL` of the script that occupied the longest frame. */
  topSource: string | null;
  /** The `invoker` of that script (what called it). */
  topInvoker: string | null;
}

/** The `playState` buckets the animation census reports. */
export type AnimationStates = Record<(typeof ANIMATION_STATES)[number], number>;

/**
 * The page facts that are cheap enough to read on every window.
 *
 * The animation census is deliberately *not* here: reading it is by far the
 * most expensive thing the sampler does, so it is taken on its own, slower
 * cadence and carries its own age.
 */
export interface RendererHealthPageGauges {
  /** `performance.timeOrigin` — the epoch milliseconds the document began at. */
  timeOriginMs: number;
  /** Elements in the document. */
  domElements: number;
  /** `performance.memory.usedJSHeapSize`, or `null` where it is unavailable. */
  jsHeapBytes: number | null;
  /** Terminals currently attached to a DOM container. */
  attachedTerminals: number;
  /** Live WebGL renderers. */
  liveWebglRenderers: number;
  /** `document.visibilityState`. */
  visibility: string;
  /** Whether the OS window has focus. */
  focused: boolean;
  /** Whether the idle animation gate is closed. */
  idle: boolean;
}

/** Point-in-time facts about the page, including the animation census. */
export interface RendererHealthGauges extends RendererHealthPageGauges {
  /** Live animations, by `playState` — from the most recent census. */
  animations: AnimationStates;
  /**
   * How old the animation census is, in milliseconds.
   *
   * The census is taken on a slower cadence than the line, so the line says
   * how stale the animation figures are rather than letting them be read as
   * if they were taken at the same instant as everything else.
   */
  animationsAgeMs: number;
}

/** Everything one health line reports. */
export interface RendererHealthSample {
  /** Wall-clock milliseconds since the epoch, when the line was produced. */
  epochMs: number;
  /** The gauges read at sample time. */
  gauges: RendererHealthGauges;
  /** Keyboard interaction latency over the window. */
  keyboard: InteractionStats;
  /** Pointer interaction latency over the window. */
  pointer: InteractionStats;
  /** Long frames over the window. */
  longFrames: LongFrameStats;
  /** Activity counters over the window. */
  activity: Record<string, number>;
  /** IPC events received over the window, highest first. */
  ipc: Array<{ name: string; count: number }>;
  /**
   * The sampler's own main-thread work for this window — the gauge reads,
   * the aggregation and the formatting — in milliseconds.
   *
   * Measured with `performance.now()`, which a WebView coarsens to 100 µs,
   * so a reading is quantised to a tenth of a millisecond and carries up to
   * 100 µs of error at each end. A Chrome trace is the precise instrument;
   * this figure is the cheap one that travels with every line.
   */
  costMs: number;
}

// ---- Formatting ----------------------------------------------------------

function round(value: number): number {
  return Math.round(value * 100) / 100;
}

/**
 * Percentile of an already-sorted ascending array, nearest-rank.
 *
 * Nearest-rank is used rather than an interpolating percentile because the
 * values are millisecond-resolution measurements of individual events: the
 * question being asked is "how bad was the worst tenth of the keystrokes",
 * and nearest-rank answers that with an actually-observed value rather than
 * a synthetic one between two observations.
 */
export function percentile(sorted: readonly number[], q: number): number {
  if (sorted.length === 0) return 0;
  const rank = Math.ceil(q * sorted.length);
  const index = Math.min(sorted.length - 1, Math.max(0, rank - 1));
  return sorted[index];
}

function statsBlock(stats: InteractionStats): string {
  return [
    `n:${stats.count}`,
    `p50:${round(stats.p50)}`,
    `p90:${round(stats.p90)}`,
    `max:${round(stats.max)}`,
    `proc_p50:${round(stats.processingP50)}`,
    `proc_p90:${round(stats.processingP90)}`,
    `proc_max:${round(stats.processingMax)}`,
    `pres_p50:${round(stats.presentationP50)}`,
    `pres_p90:${round(stats.presentationP90)}`,
    `pres_max:${round(stats.presentationMax)}`,
  ].join('/');
}

/**
 * Strip whitespace and cap the length of a free-text field value.
 *
 * A newline in a script URL would split the line in two and turn one
 * diagnostic into two unreadable ones.
 */
export function sanitizeFieldValue(value: string, cap = 160): string {
  const collapsed = value.replace(/\s+/g, '_').trim();
  return collapsed.length <= cap ? collapsed : `${collapsed.slice(0, cap)}…`;
}

/**
 * Format one health line.
 *
 * Pure: it takes a sample and returns a string, so the format can be pinned
 * by a unit test without a browser, a timer or an IPC hop.
 */
export function formatRendererHealthLine(sample: RendererHealthSample): string {
  const { gauges } = sample;
  const animations = ANIMATION_STATES.map(state => `${state}:${gauges.animations[state] ?? 0}`).join('/');
  const heap = gauges.jsHeapBytes === null ? 'off' : String(round(gauges.jsHeapBytes / (1024 * 1024)));
  const activity = Object.entries(sample.activity)
    .map(([kind, count]) => `${kind}:${count}`)
    .join('/');
  const ipc = sample.ipc.map(entry => `${sanitizeFieldValue(entry.name, 64)}:${entry.count}`).join(',');

  const fields = [
    `age_ms=${Math.max(0, Math.round(sample.epochMs - gauges.timeOriginMs))}`,
    `time_origin=${round(gauges.timeOriginMs)}`,
    `vis=${sanitizeFieldValue(gauges.visibility, 16)}`,
    `focus=${gauges.focused ? 1 : 0}`,
    `idle=${gauges.idle ? 1 : 0}`,
    `dom=${gauges.domElements}`,
    `anim=${animations}`,
    `anim_age_ms=${Math.max(0, Math.round(gauges.animationsAgeMs))}`,
    `heap_mb=${heap}`,
    `term=${gauges.attachedTerminals}`,
    `webgl=${gauges.liveWebglRenderers}`,
    `ev_kb=${statsBlock(sample.keyboard)}`,
    `ev_ptr=${statsBlock(sample.pointer)}`,
    `lf=n:${sample.longFrames.count}/total_ms:${round(sample.longFrames.totalMs)}/max_ms:${round(sample.longFrames.maxMs)}`,
    `act=${activity}`,
    `ipc=${ipc}`,
    `cost_ms=${round(sample.costMs)}`,
  ];

  if (sample.longFrames.topSource !== null) {
    fields.push(`lf_src=${sanitizeFieldValue(sample.longFrames.topSource)}`);
  }
  if (sample.longFrames.topInvoker !== null) {
    fields.push(`lf_invoker=${sanitizeFieldValue(sample.longFrames.topInvoker, 80)}`);
  }

  return `${RENDER_HEALTH_PREFIX} ${fields.join(' ')}`;
}

// ---- Collection ----------------------------------------------------------

interface EventTimingEntry {
  name?: string;
  startTime?: number;
  processingStart?: number;
  processingEnd?: number;
  duration?: number;
}

interface ScriptTiming {
  sourceURL?: string;
  sourceFunctionName?: string;
  invoker?: string;
  invokerType?: string;
  duration?: number;
}

interface LongAnimationFrameEntry {
  duration?: number;
  startTime?: number;
  scripts?: ScriptTiming[];
}

interface LongTaskAttribution {
  name?: string;
  containerType?: string;
  containerSrc?: string;
}

interface LongTaskEntry {
  name?: string;
  duration?: number;
  attribution?: LongTaskAttribution[];
}

function numberOr(value: unknown, fallback = 0): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

/** A bounded list of retained measurements. */
class SampleWindow {
  private readonly values: number[] = [];
  private observed = 0;

  push(value: number): void {
    this.observed += 1;
    this.values.push(value);
    if (this.values.length > MAX_RETAINED_SAMPLES) {
      // Drop the oldest: the percentile is over the most recent window.
      this.values.splice(0, this.values.length - MAX_RETAINED_SAMPLES);
    }
  }

  get count(): number {
    return this.observed;
  }

  sorted(): number[] {
    return [...this.values].sort((a, b) => a - b);
  }

  reset(): void {
    this.values.length = 0;
    this.observed = 0;
  }
}

interface InteractionWindow {
  input: SampleWindow;
  processing: SampleWindow;
  presentation: SampleWindow;
}

function emptyInteractionWindow(): InteractionWindow {
  return { input: new SampleWindow(), processing: new SampleWindow(), presentation: new SampleWindow() };
}

function interactionStats(window: InteractionWindow): InteractionStats {
  const input = window.input.sorted();
  const processing = window.processing.sorted();
  const presentation = window.presentation.sorted();
  return {
    count: window.input.count,
    p50: percentile(input, 0.5),
    p90: percentile(input, 0.9),
    max: input.length > 0 ? input[input.length - 1] : 0,
    processingP50: percentile(processing, 0.5),
    processingP90: percentile(processing, 0.9),
    processingMax: processing.length > 0 ? processing[processing.length - 1] : 0,
    presentationP50: percentile(presentation, 0.5),
    presentationP90: percentile(presentation, 0.9),
    presentationMax: presentation.length > 0 ? presentation[presentation.length - 1] : 0,
  };
}

// ---- The sampler ---------------------------------------------------------

/** The seams the sampler is built on, so a test can drive it without a browser. */
export interface RendererHealthSamplerOptions {
  /** How often a line is written. */
  intervalMs?: number;
  /** Monotonic clock in milliseconds. */
  now?: () => number;
  /** Wall clock in milliseconds since the epoch. */
  epochNow?: () => number;
  /** Arm a one-shot timer. */
  schedule?: (callback: () => void, ms: number) => unknown;
  /** Cancel a timer armed by `schedule`. */
  cancel?: (handle: unknown) => void;
  /**
   * Install an observer for one entry type.
   *
   * Returns a teardown function, or `null` when the entry type is not
   * supported by this browser.
   */
  observe?: (type: string, callback: (entries: readonly unknown[]) => void) => (() => void) | null;
  /** Read the point-in-time page facts. */
  readGauges?: () => RendererHealthPageGauges;
  /**
   * Take the animation census.
   *
   * Separate from `readGauges` because it is expensive enough to deserve its
   * own cadence: the sampler calls this on the census interval, not on every
   * window.
   */
  readAnimationStates?: () => AnimationStates;
  /** How often the animation census is taken, in milliseconds. */
  animationCensusIntervalMs?: number;
  /** Read and clear the activity counters. */
  readActivity?: () => Record<string, number>;
  /** Read and clear the IPC event counts. */
  readIpcEvents?: () => Array<{ name: string; count: number }>;
  /** Write the line. */
  emit?: (line: string) => void;
  /** Subscribe to OS window focus changes. */
  subscribeFocus?: (onChange: (focused: boolean) => void) => () => void;
}

/** How many IPC event names the line reports. */
export const IPC_TOP_N = 5;

function defaultObserve(type: string, callback: (entries: readonly unknown[]) => void): (() => void) | null {
  if (typeof PerformanceObserver === 'undefined') return null;
  let observer: PerformanceObserver;
  try {
    observer = new PerformanceObserver(list => callback(list.getEntries()));
  } catch {
    return null;
  }
  try {
    if (type === 'event') {
      // `durationThreshold` is part of the Event Timing extension, which the
      // bundled TypeScript lib does not declare yet. The browser accepts it;
      // a browser that does not throws here and the observer is skipped.
      observer.observe({
        type: 'event',
        durationThreshold: EVENT_TIMING_THRESHOLD_MS,
        buffered: true,
      } as PerformanceObserverInit);
    } else {
      observer.observe({ type, buffered: true });
    }
  } catch {
    // The browser does not support this entry type. Reporting that is the
    // caller's job — the long-frame path falls back to `longtask`.
    return null;
  }
  return () => observer.disconnect();
}

function emptyAnimationStates(): AnimationStates {
  return { running: 0, paused: 0, finished: 0, idle: 0, pending: 0 };
}

function readAnimationStates(): AnimationStates {
  const counts = emptyAnimationStates();
  try {
    const animations = typeof document.getAnimations === 'function' ? document.getAnimations() : [];
    for (const animation of animations) {
      const state = animation.playState as (typeof ANIMATION_STATES)[number];
      if (state in counts) counts[state] += 1;
    }
  } catch {
    // `getAnimations` is Chromium-only and can throw on a torn-down
    // document. An unavailable gauge reads as zero, not as a failure.
  }
  return counts;
}

function readJsHeapBytes(): number | null {
  try {
    const memory = (performance as unknown as { memory?: { usedJSHeapSize?: number } }).memory;
    const used = memory?.usedJSHeapSize;
    return typeof used === 'number' && Number.isFinite(used) ? used : null;
  } catch {
    return null;
  }
}

/**
 * The sampler.
 *
 * One instance per window. `start()` installs the observers and arms the
 * timer; `stop()` releases both.
 */
export class RendererHealthSampler {
  private readonly options: Required<Omit<RendererHealthSamplerOptions, 'subscribeFocus'>> &
    Pick<RendererHealthSamplerOptions, 'subscribeFocus'>;

  private readonly keyboard = emptyInteractionWindow();
  private readonly pointer = emptyInteractionWindow();

  private longFrameCount = 0;
  private longFrameTotalMs = 0;
  private longFrameMaxMs = 0;
  private longFrameSource: string | null = null;
  private longFrameInvoker: string | null = null;

  private timer: unknown = null;
  private running = false;
  /** Observer teardowns, released by `stop()`. */
  private teardowns: Array<() => void> = [];
  private focused = true;
  /**
   * Whether the OS focus fact has been read at all. Until it has, the
   * `document.hasFocus()` fallback is the answer; after it has, the OS fact
   * wins — a WebView that still reports `document.hasFocus() === true`
   * while the window is in the background must not read as focused.
   */
  private focusKnown = false;
  private unlistenFocus: (() => void) | null = null;
  /** The most recent animation census. */
  private animationStates: AnimationStates = emptyAnimationStates();
  /** When the census was taken, on the sampler's monotonic clock. */
  private animationCensusAtMs: number | null = null;

  constructor(options: RendererHealthSamplerOptions = {}) {
    this.options = {
      intervalMs: options.intervalMs ?? DEFAULT_HEALTH_INTERVAL_MS,
      now: options.now ?? (() => performance.now()),
      epochNow: options.epochNow ?? (() => Date.now()),
      schedule: options.schedule ?? ((callback, ms) => setTimeout(callback, ms)),
      cancel: options.cancel ?? (handle => clearTimeout(handle as ReturnType<typeof setTimeout>)),
      observe: options.observe ?? defaultObserve,
      readGauges: options.readGauges ?? (() => defaultReadGauges(() => this.focused, () => this.focusKnown)),
      readAnimationStates: options.readAnimationStates ?? (() => readAnimationStates()),
      animationCensusIntervalMs: options.animationCensusIntervalMs ?? DEFAULT_ANIMATION_CENSUS_INTERVAL_MS,
      readActivity: options.readActivity ?? (() => readAndResetRendererActivity()),
      readIpcEvents: options.readIpcEvents ?? (() => readAndResetIpcEventCounts()),
      emit: options.emit ?? (line => logFrontend('info', line)),
      subscribeFocus: options.subscribeFocus,
    };
  }

  /** Install the observers and arm the timer. Idempotent. */
  start(): void {
    if (this.running) return;
    this.running = true;

    this.installObserver('event', entries => this.collectEventTiming(entries));

    // `long-animation-frame` is the modern entry type and it names the
    // script that occupied the frame. `longtask` is the older fallback and
    // only names a container — the fallback keeps the count and the
    // duration, which is the part that says "the renderer stalled".
    const longFrameHandler = (entries: readonly unknown[]) => this.collectLongFrames(entries);
    if (!this.installObserver('long-animation-frame', longFrameHandler)) {
      this.installObserver('longtask', longFrameHandler);
    }

    if (this.options.subscribeFocus) {
      this.unlistenFocus = this.options.subscribeFocus(focused => {
        this.focused = focused;
        this.focusKnown = true;
      });
    }

    this.arm();
  }

  /**
   * Install one observer, keeping its teardown so `stop()` can release it.
   *
   * Returns `false` when the entry type is not supported, so the caller can
   * fall back. An observer that throws on installation is treated as
   * unsupported: a browser that cannot report one kind of timing must not
   * stop the sampler from reporting the others.
   */
  private installObserver(
    type: string,
    callback: (entries: readonly unknown[]) => void,
  ): boolean {
    let teardown: (() => void) | null = null;
    try {
      teardown = this.options.observe(type, callback);
    } catch {
      return false;
    }
    if (!teardown) return false;
    this.teardowns.push(teardown);
    return true;
  }

  /** Release the timer, the observers and the focus subscription. Idempotent. */
  stop(): void {
    if (!this.running) return;
    this.running = false;
    if (this.timer !== null) {
      this.options.cancel(this.timer);
      this.timer = null;
    }
    for (const teardown of this.teardowns) {
      try {
        teardown();
      } catch {
        // A teardown that throws must not stop the others being released.
      }
    }
    this.teardowns = [];
    if (this.unlistenFocus) {
      this.unlistenFocus();
      this.unlistenFocus = null;
    }
  }

  private arm(): void {
    if (!this.running) return;
    this.timer = this.options.schedule(() => {
      this.timer = null;
      // A timer that fires after `stop()` must not write a line: the caller
      // asked for the sampler to be gone, and a line after that point is a
      // record of a session state that no longer exists.
      if (!this.running) return;
      this.sample();
      this.arm();
    }, this.options.intervalMs);
  }

  /**
   * Collect one window, format it, and write the line.
   *
   * Public so a test can drive a window without a timer, and so the
   * dev-profile check can force a line instead of waiting for the next one.
   */
  sample(): string {
    const started = this.options.now();

    const gauges = this.readGaugesWithCensus(started);
    const keyboard = interactionStats(this.keyboard);
    const pointer = interactionStats(this.pointer);
    const longFrames: LongFrameStats = {
      count: this.longFrameCount,
      totalMs: this.longFrameTotalMs,
      maxMs: this.longFrameMaxMs,
      topSource: this.longFrameSource,
      topInvoker: this.longFrameInvoker,
    };
    const activity = this.options.readActivity();
    const ipc = this.options.readIpcEvents().slice(0, IPC_TOP_N);

    const rawLine = formatRendererHealthLine({
      epochMs: this.options.epochNow(),
      gauges,
      keyboard,
      pointer,
      longFrames,
      activity,
      ipc,
      // Filled in below: the format needs the figure, and the figure is
      // only known once the format is done.
      costMs: 0,
    });

    this.resetWindow();

    const finished = this.options.now();
    const line = this.withCost(rawLine, finished - started);
    this.options.emit(line);
    return line;
  }

  /**
   * Read the page gauges, taking the animation census only when it is due.
   *
   * The census is the expensive read, so it is taken on its own cadence and
   * the line reports its age. Everything else is cheap enough to read on
   * every window.
   */
  private readGaugesWithCensus(nowMs: number): RendererHealthGauges {
    const due =
      this.animationCensusAtMs === null ||
      nowMs - this.animationCensusAtMs >= this.options.animationCensusIntervalMs;
    if (due) {
      this.animationStates = this.options.readAnimationStates();
      this.animationCensusAtMs = nowMs;
    }
    return {
      ...this.options.readGauges(),
      animations: this.animationStates,
      animationsAgeMs:
        this.animationCensusAtMs === null ? 0 : Math.max(0, nowMs - this.animationCensusAtMs),
    };
  }

  /**
   * Replace the placeholder cost with the measured one.
   *
   * The cost cannot be known before the line is formatted, so the line is
   * formatted with a placeholder and the field is rewritten afterwards. One
   * clock pair brackets the whole sample, because a WebView coarsens
   * `performance.now()` to 100 µs — two extra clock reads would cost more
   * than the work they measure.
   */
  private withCost(line: string, costMs: number): string {
    return line.replace(/cost_ms=[\d.]+/, `cost_ms=${round(costMs)}`);
  }

  private resetWindow(): void {
    this.keyboard.input.reset();
    this.keyboard.processing.reset();
    this.keyboard.presentation.reset();
    this.pointer.input.reset();
    this.pointer.processing.reset();
    this.pointer.presentation.reset();
    this.longFrameCount = 0;
    this.longFrameTotalMs = 0;
    this.longFrameMaxMs = 0;
    this.longFrameSource = null;
    this.longFrameInvoker = null;
  }

  /** Fold a batch of Event Timing entries into the keyboard / pointer splits. */
  private collectEventTiming(entries: readonly unknown[]): void {
    for (const raw of entries) {
      const entry = raw as EventTimingEntry;
      const name = typeof entry.name === 'string' ? entry.name : '';
      const window = KEYBOARD_EVENT_NAMES.has(name)
        ? this.keyboard
        : POINTER_EVENT_NAMES.has(name)
          ? this.pointer
          : null;
      if (window === null) continue;

      const startTime = numberOr(entry.startTime);
      const processingStart = numberOr(entry.processingStart);
      const processingEnd = numberOr(entry.processingEnd);
      const duration = numberOr(entry.duration);

      window.input.push(Math.max(0, processingStart - startTime));
      window.processing.push(Math.max(0, processingEnd - processingStart));
      window.presentation.push(Math.max(0, startTime + duration - processingEnd));
    }
  }

  /** Fold a batch of long frames (LoAF or the `longtask` fallback) in. */
  private collectLongFrames(entries: readonly unknown[]): void {
    for (const raw of entries) {
      const entry = raw as LongAnimationFrameEntry & LongTaskEntry;
      const duration = numberOr(entry.duration);
      this.longFrameCount += 1;
      this.longFrameTotalMs += duration;
      if (duration > this.longFrameMaxMs) {
        this.longFrameMaxMs = duration;
        const attribution = attributeLongFrame(entry);
        this.longFrameSource = attribution.source;
        this.longFrameInvoker = attribution.invoker;
      }
    }
  }
}

/**
 * Name the script that occupied a long frame.
 *
 * A `long-animation-frame` entry carries the scripts it ran, with their
 * `sourceURL` and `invoker`; the `longtask` fallback carries only a
 * container attribution. The longest script of the longest frame is the one
 * worth naming.
 */
function attributeLongFrame(entry: LongAnimationFrameEntry & LongTaskEntry): { source: string | null; invoker: string | null } {
  const scripts = Array.isArray(entry.scripts) ? entry.scripts : [];
  let longest: ScriptTiming | null = null;
  for (const script of scripts) {
    if (longest === null || numberOr(script.duration) > numberOr(longest.duration)) longest = script;
  }
  if (longest) {
    return {
      source: longest.sourceURL ?? null,
      invoker: longest.invoker ?? longest.invokerType ?? longest.sourceFunctionName ?? null,
    };
  }
  const attribution = Array.isArray(entry.attribution) ? entry.attribution : [];
  const first = attribution.find(item => item && (item.containerSrc || item.name));
  if (!first) return { source: null, invoker: null };
  return {
    source: first.containerSrc ?? first.name ?? null,
    invoker: first.containerType ?? null,
  };
}

// ---- Default gauges ------------------------------------------------------

function defaultReadGauges(focused: () => boolean, focusKnown: () => boolean): RendererHealthPageGauges {
  let domElements = 0;
  try {
    domElements = document.getElementsByTagName('*').length;
  } catch {
    domElements = 0;
  }

  let visibility = 'unknown';
  try {
    visibility = document.visibilityState;
  } catch {
    visibility = 'unknown';
  }

  let idle = false;
  try {
    idle = document.documentElement.getAttribute(IDLE_ROOT_ATTRIBUTE) === 'true';
  } catch {
    idle = false;
  }

  let focusedState = false;
  try {
    focusedState = document.hasFocus();
  } catch {
    focusedState = false;
  }

  return {
    timeOriginMs: numberOr(performance.timeOrigin, Date.now()),
    domElements,
    jsHeapBytes: readJsHeapBytes(),
    attachedTerminals: attachedTerminalCount(),
    liveWebglRenderers: liveWebglRendererCount(),
    visibility,
    // The OS fact wins once it has been read. Until then the DOM's answer
    // is the best available one — a WebView that still reports
    // `document.hasFocus() === true` while the window is in the background
    // must not be able to override the OS.
    focused: focusKnown() ? focused() : focusedState,
    idle,
  };
}

// ---- Wiring --------------------------------------------------------------

/**
 * Subscribe to the OS focus fact.
 *
 * `watchWindowFocus` is the sanctioned reader: it answers the initial
 * question and every transition. The **initial answer is not a change** —
 * a window that was already unfocused when the page loaded must not be
 * reported as a focus change — so only a transition off a known previous
 * state is counted.
 *
 * Outside a Tauri runtime there is no OS focus to read, so nothing is
 * subscribed: the gauge falls back to `document.hasFocus()` and the
 * browser dev server does not print a warning about a window it never had.
 */
function defaultSubscribeFocus(onChange: (focused: boolean) => void): () => void {
  if (!isTauriRuntime()) return () => {};
  let last: boolean | null = null;
  return watchWindowFocus(focused => {
    const isTransition = last !== null && last !== focused;
    last = focused;
    onChange(focused);
    if (isTransition) bumpRendererActivity('focus_change');
  });
}

let activeSampler: RendererHealthSampler | null = null;

/**
 * Start the renderer health sampler for this window.
 *
 * Called once at boot. Returns a function that stops it.
 */
export function startRendererHealthSampler(options: RendererHealthSamplerOptions = {}): () => void {
  if (activeSampler) return () => stopRendererHealthSampler();
  const sampler = new RendererHealthSampler({
    ...options,
    subscribeFocus: options.subscribeFocus ?? defaultSubscribeFocus,
  });
  activeSampler = sampler;
  sampler.start();
  return () => stopRendererHealthSampler();
}

/** Stop the running sampler, if any. */
export function stopRendererHealthSampler(): void {
  if (!activeSampler) return;
  activeSampler.stop();
  activeSampler = null;
}

/** Test-only: the running sampler. */
export function __activeRendererHealthSamplerForTests(): RendererHealthSampler | null {
  return activeSampler;
}
