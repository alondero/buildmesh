/**
 * GPU context-loss crash mitigation (2026-08-26 diagnosis). Every xterm
 * that loads the WebGL renderer holds a live WebGL context. Chromium caps
 * active WebGL contexts at ~16 per process; Buildmesh routinely runs 15+
 * agent terminals plus build/run panes, so context creation was constantly
 * evicting older contexts — the repeated "webgl context not restored;
 * firing onContextLoss" warnings in buildmesh.log — churning the GPU right
 * before the NVIDIA driver resets that killed the whole app.
 *
 * The pool caps live WebGL contexts at a small LRU window (default 4).
 * Only the most recently attached terminals get the GPU renderer; the rest
 * fall back to xterm's DOM renderer, which costs almost nothing for
 * non-visible panes. Registries call `activate` on DOM attach and
 * `release` on detach/dispose.
 *
 * Keys are namespaced by the caller (`agent:${nodeId}` vs
 * `buildRun:${sessionId}`) because the two registries use colliding numeric
 * id spaces — same reason their writers are separate registries.
 *
 * Issue #1568 — `loadWebglRenderer.ts` itself dynamic-imports
 * `@xterm/addon-webgl`, so the WebGL renderer chunk only loads when the
 * first WebGL attach fires. `activate` / `release` stay synchronous here;
 * `loadWebglRenderer`'s internal Promise handles the case where
 * `release()` runs before the WebGL addon has been constructed (see
 * `loadWebglRenderer.ts` for the deferred-dispose path).
 */
import type { Terminal } from '@xterm/xterm';
import { loadWebglRenderer } from './loadWebglRenderer';

export const MAX_ACTIVE_WEBGL_CONTEXTS = 4;

interface PoolEntry {
  term: Terminal;
  /** Disposes the WebglAddon (idempotent — no-op after context loss). */
  detachRenderer: () => void;
}

export class WebglRendererPool {
  private entries = new Map<string, PoolEntry>();
  /** Insertion-ordered map doubles as the LRU list: first key = oldest. */
  private lru: string[] = [];

  constructor(private readonly maxActive: number = MAX_ACTIVE_WEBGL_CONTEXTS) {}

  /** Attach/promote `key`. Evicts the least-recently-used entry over budget. */
  activate(key: string, term: Terminal): void {
    // Promotion path: already holding this key — just bump it to MRU.
    if (this.entries.has(key)) {
      this.touch(key);
      return;
    }
    // Insertion path: make room FIRST (while at capacity, not over it),
    // then claim the MRU slot so the new entry can't evict itself.
    while (this.entries.size >= this.maxActive) {
      const evict = this.lru[0];
      if (evict === undefined) break;
      this.release(evict);
    }
    this.touch(key);
    const detachRenderer = loadWebglRenderer(term);
    this.entries.set(key, { term, detachRenderer });
  }

  /** Drop `key` from the pool, disposing its WebGL addon if still held. */
  release(key: string): void {
    this.lru = this.lru.filter((k) => k !== key);
    const entry = this.entries.get(key);
    if (!entry) return;
    this.entries.delete(key);
    try {
      entry.detachRenderer();
    } catch {
      // addon already gone (context loss) — ignore
    }
  }

  releaseAll(): void {
    for (const key of [...this.lru]) {
      this.release(key);
    }
  }

  size(): number {
    return this.entries.size;
  }

  has(key: string): boolean {
    return this.entries.has(key);
  }

  private touch(key: string): void {
    this.lru = this.lru.filter((k) => k !== key);
    this.lru.push(key);
  }
}

/**
 * Process-wide singleton so the cap spans BOTH terminal registries —
 * the GPU sees one Chromium context budget regardless of which registry
 * a pane belongs to.
 */
export const terminalWebglPool = new WebglRendererPool();
