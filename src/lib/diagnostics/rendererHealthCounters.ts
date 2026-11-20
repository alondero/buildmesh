/**
 * Activity counters for the renderer health line (issue #2833).
 *
 * ## Why these live outside a store
 *
 * The counters exist only to be read once every 10 s and turned into one
 * diagnostic line. Putting them in a Zustand store would be wrong twice
 * over: a store write notifies subscribers, so counting a *view switch* would
 * wake every `useUIStore` consumer for a fact no consumer reads — and the
 * `setViewMode` idempotency contract ("a same-mode call notifies nobody")
 * would be broken by the counter itself. They are module-level plain numbers
 * instead: a bump is a `+= 1` with no notification, no allocation and no
 * subscriber, which is what a 10-second sampler needs.
 *
 * ## Counters vs. gauges
 *
 * A **counter** is an event that happened during the sample window and is
 * read-and-reset at each line ("how many view switches since the last line").
 * A **gauge** is a fact about right now ("how many terminals are attached
 * now"). Counters live here; gauges are *providers* registered by the module
 * that owns the fact, because the sampler must not import a component module
 * to read a terminal count — the dependency has to point the other way.
 *
 * ## Cost
 *
 * The bump path is O(1) and allocation-free: a counter is a `+= 1` on a plain
 * number, with no notification and no subscriber. The read path allocates one
 * small object per window, which is the price of handing the sampler a
 * snapshot it can format without reading the live table. The IPC event names
 * are counted in the transport wrapper (`src/lib/tauri/eventListen.ts`),
 * which is the one place that sees a delivered event.
 */

/** One counted renderer activity. */
export type RendererActivityKind =
  /** A canvas View Mode switch (`single` / `mesh` / `pinned` / `all`). */
  | 'view_switch'
  /** A terminal was attached to a DOM container. */
  | 'terminal_attach'
  /** A terminal was detached from its DOM container. */
  | 'terminal_detach'
  /** The WebGL pool evicted a renderer to stay inside its context budget. */
  | 'webgl_eviction'
  /** A node status transition was applied to the agent-node store. */
  | 'node_status'
  /** A modal dialog was opened. */
  | 'dialog'
  /** A toast was opened. */
  | 'toast'
  /** The OS window gained or lost focus. */
  | 'focus_change';

/**
 * Every activity kind, in the order the health line reports them. The order
 * is part of the line format, so it is declared once here rather than being
 * restated by the formatter.
 */
export const RENDERER_ACTIVITY_KINDS: readonly RendererActivityKind[] = [
  'view_switch',
  'terminal_attach',
  'terminal_detach',
  'webgl_eviction',
  'node_status',
  'dialog',
  'toast',
  'focus_change',
];

/** The counter values for one sample window. */
export type RendererActivityCounts = Record<RendererActivityKind, number>;

function emptyCounts(): RendererActivityCounts {
  return {
    view_switch: 0,
    terminal_attach: 0,
    terminal_detach: 0,
    webgl_eviction: 0,
    node_status: 0,
    dialog: 0,
    toast: 0,
    focus_change: 0,
  };
}

const counts: RendererActivityCounts = emptyCounts();

/**
 * Count one activity.
 *
 * `by` is a plain add so a caller that already knows it handled N events
 * (a batch of node status patches, say) does not need N calls.
 */
export function bumpRendererActivity(kind: RendererActivityKind, by = 1): void {
  if (!Number.isFinite(by) || by <= 0) return;
  counts[kind] += by;
}

/** Read the counters without clearing them (diagnostics, tests). */
export function readRendererActivity(): RendererActivityCounts {
  return { ...counts };
}

/**
 * Read the counters and clear them.
 *
 * The sampler calls this once per line: the line reports what happened
 * *since the last line*, which is the window the issue asks for.
 */
export function readAndResetRendererActivity(): RendererActivityCounts {
  const snapshot = { ...counts };
  for (const kind of RENDERER_ACTIVITY_KINDS) counts[kind] = 0;
  return snapshot;
}

/** Test-only: clear every counter. */
export function resetRendererActivity(): void {
  const zeroed = emptyCounts();
  for (const kind of RENDERER_ACTIVITY_KINDS) counts[kind] = zeroed[kind];
}

// ---- Gauges -------------------------------------------------------------
//
// A gauge provider is a function the owning module registers. The sampler
// only ever calls the provider, so it never has to know where the fact comes
// from — and the owner keeps its own bookkeeping instead of the sampler
// reaching into a component.

type GaugeProvider = () => number;

const attachedTerminalProviders = new Map<string, GaugeProvider>();
const liveWebglProviders = new Map<string, GaugeProvider>();

/**
 * Register a provider of "how many terminals are attached to the DOM right
 * now".
 *
 * One provider per terminal registry. The key is only used to keep the
 * registrations apart; the count is the sum.
 */
export function registerAttachedTerminalCounter(key: string, provider: GaugeProvider): void {
  attachedTerminalProviders.set(key, provider);
}

/** The number of terminals currently attached to a DOM container. */
export function attachedTerminalCount(): number {
  return sumProviders(attachedTerminalProviders);
}

/**
 * Register a provider of "how many live WebGL renderers exist right now".
 *
 * One provider per WebGL pool.
 */
export function registerLiveWebglCounter(key: string, provider: GaugeProvider): void {
  liveWebglProviders.set(key, provider);
}

/** The number of live WebGL renderers held by the registered pools. */
export function liveWebglRendererCount(): number {
  return sumProviders(liveWebglProviders);
}

function sumProviders(providers: Map<string, GaugeProvider>): number {
  let total = 0;
  for (const provider of providers.values()) {
    try {
      const value = provider();
      if (Number.isFinite(value)) total += value;
    } catch {
      // A broken gauge must not take the whole health line down: the line is
      // the record of a session that is already going wrong.
    }
  }
  return total;
}
