import type { Terminal } from '@xterm/xterm';

// Issue #1568 — `@xterm/addon-webgl` carries its own WebGL renderer
// (≈80 kB minified) and a native WebGL context per attach. Issue #1122's
// pool only mounts a WebGL renderer on the most-recently-attached panes,
// so we lazy-import the addon inside the loader and let Vite emit it as a
// separate chunk fetched on first attach. The module is cached in a
// module-level promise after the first call so subsequent
// `loadWebglRenderer` invocations don't re-fetch the chunk AND don't
// re-trigger the dynamic-import resolution path that can otherwise
// bypass the vitest mock when run multiple times in the same process.

/**
 * Attaches the WebGL renderer to an xterm.js Terminal.
 *
 * Issue #1122: xterm's default DOM renderer recreates a `<span>` per cell on
 * every redraw. With the 10k-line scrollback Buildmesh retains, a busy agent
 * TUI accumulates hundreds of thousands of spans, and the per-keystroke
 * cursor-positioning escapes (which clear + re-style cells) force a full
 * reflow. After ~30 minutes of agent output the renderer can take 30-60ms
 * per frame, which is the longest tail in the round-trip latency chain.
 *
 * The WebGL renderer sidesteps the DOM by drawing the entire grid to a
 * single canvas each frame, so per-cell work is O(1) text glyphs uploaded
 * to a texture, not O(cells) `<span>` operations. The win is largest where
 * Buildmesh hits it hardest: large scrollback + many styled spans.
 *
 * Fallback ladder: WebGL → DOM. WebGL is the only renderer that materially
 * fixes the latency; the canvas addon is incompatible with xterm 6.0 (its
 * 0.7.x peer pin is `^5.0.0`), so the DOM renderer is the reliable fallback
 * when WebGL is unavailable (headless WebView2, no GPU, drawn on a remote
 * desktop, etc.) or its context is lost after attach (e.g. the user docked
 * their laptop and the GPU driver reset).
 *
 * Disposes the WebGL addon on context loss so the Terminal keeps working
 * with the DOM renderer it had before we tried to upgrade. Returns a
 * disposer so callers (the WebglRendererPool) can drop the addon when the
 * terminal no longer warrants a GPU context; the disposer is idempotent
 * and a safe no-op after a context loss. Tests can stub the addon via
 * `vi.mock('@xterm/addon-webgl', ...)`.
 */
// Module-level cache of the `@xterm/addon-webgl` dynamic import. The
// first `loadWebglRenderer` call resolves this; every subsequent call
// reuses the cached module without going through the dynamic-import
// resolver again. This keeps the chunk-fetch path off the hot path AND
// matches how vitest's `vi.mock` expects to see the module (once,
// resolved at module load).
let addonModulePromise: Promise<typeof import('@xterm/addon-webgl')> | null = null;
function getAddonModule(): Promise<typeof import('@xterm/addon-webgl')> {
  if (addonModulePromise === null) {
    addonModulePromise = import('@xterm/addon-webgl');
  }
  return addonModulePromise;
}

export function loadWebglRenderer(term: Terminal): () => void {
  let webgl: import('@xterm/addon-webgl').WebglAddon | null = null;
  // Set once the addon is gone (explicit dispose or context loss) so the
  // disposer never double-disposes.
  let released = false;
  let detachChain: Promise<void> | null = null;

  const detach = () => {
    if (released) return;
    released = true;
    // If the addon hasn't been constructed yet, defer the dispose to
    // the attach promise so a fast `release()` call doesn't leave a
    // dangling renderer. The WebGL attach is idempotent because
    // `loadWebglRenderer` is the only writer.
    if (webgl === null) {
      detachChain = (detachChain ?? getAddonModule())?.then(() => {
        try {
          webgl?.dispose(); // allow-dispose — WebGL addon only, not the Terminal itself
        } catch {
          // already disposed — ignore
        }
        webgl = null;
      });
      return;
    }
    try {
      webgl.dispose(); // allow-dispose — WebGL addon only, not the Terminal itself
    } catch {
      // already disposed — ignore
    }
    webgl = null;
  };

  getAddonModule().then(({ WebglAddon }) => {
    // A release() between the import start and its resolution leaves
    // `released === true`; skip the attach in that case.
    if (released) return;
    try {
      webgl = new WebglAddon();
      // `onContextLoss` fires before the WebGL context is fully gone. We
      // dispose the addon on the same tick so xterm falls back to the DOM
      // renderer for the next frame; a transient context loss is usually
      // permanent (driver reset, RDP reconnect), so re-loading is wasteful.
      webgl.onContextLoss(() => {
        try {
          webgl?.dispose(); // allow-dispose — WebGL addon only, not the Terminal itself
        } catch {
          // already disposed — ignore
        }
        webgl = null;
        released = true;
      });
      term.loadAddon(webgl);
    } catch (err) {
      // WebGL not supported on this host (headless, software rendering,
      // remote desktop without GPU passthrough). xterm's DOM renderer is
      // the documented fallback and what we hand back to the user in that
      // case.
      console.warn('[Terminal] WebGL renderer unavailable, using DOM fallback:', err);
      if (webgl) {
        try { webgl.dispose(); } catch { /* ignore */ } // allow-dispose — WebGL addon only, not the Terminal itself
      }
      released = true;
    }
  }).catch((err) => {
    // Dynamic-import resolution failed (network blip, chunk missing).
    // Fall through to the DOM renderer; the term is still usable.
    console.warn('[Terminal] WebGL addon chunk failed to load, using DOM fallback:', err);
    released = true;
  });

  return detach;
}
