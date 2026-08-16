import { Terminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';

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
 * with the DOM renderer it had before we tried to upgrade. Tests can
 * stub the addon via `vi.mock('@xterm/addon-webgl', ...)`.
 */
export function loadWebglRenderer(term: Terminal): void {
  let webgl: WebglAddon | null = null;
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
    });
    term.loadAddon(webgl);
  } catch (err) {
    // WebGL not supported on this host (headless, software rendering, remote
    // desktop without GPU passthrough). xterm's DOM renderer is the documented
    // fallback and what we hand back to the user in that case.
    console.warn('[Terminal] WebGL renderer unavailable, using DOM fallback:', err);
    if (webgl) {
      try { webgl.dispose(); } catch { /* ignore */ } // allow-dispose — WebGL addon only, not the Terminal itself
    }
  }
}
