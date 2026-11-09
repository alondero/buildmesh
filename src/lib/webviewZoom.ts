/**
 * Window zoom — the 200% enlargement path (issue #2739).
 *
 * The interface text scale (`textScale.ts`) tops out at 150%: past that the
 * dense chrome starts clipping rather than reflowing, and this app has no
 * room to reflow — several terminals share one window. The issue asks for a
 * *supported* way to reach 200%, and the answer is the platform's own zoom:
 * `set_webview_zoom` scales the whole webview, so the chrome, the labels
 * and the content all grow together and nothing has to be re-flowed to fit.
 *
 * This is deliberately NOT an extension of the text slider. The slider moves
 * type; the window zoom moves everything, which is a different promise and
 * a different mechanism.
 *
 * It is also deliberately NOT a terminal setting. xterm.js keeps its own
 * 8–18px font size from the title bar's Zoom control, and the app already
 * claims Ctrl/Cmd + 0 / + / − for that (issue #667) — which is why the
 * platform's own zoom hotkeys are left off and the window zoom is set from
 * Settings instead. See docs/user-guide.md.
 *
 * Persistence follows the other per-machine presentation choices: a
 * localStorage key read at module load, applied at boot by main.tsx.
 *
 * Two values, with one rule between them:
 *
 *   - the STORED factor is the last zoom the platform accepted. It is written
 *     only after the platform has applied a zoom, so it never records a size
 *     the window was not at.
 *   - the value the Settings control shows is the zoom the window is at right
 *     now.
 *
 * They agree after a successful boot. A platform that refuses the zoom
 * (Android does not implement the command; a mis-built binary may lack the
 * capability) must not leave the radio claiming 200% while the window is still
 * at 100% — so a refusal publishes nothing, leaves the control showing the
 * truth, and reports the failure to the caller. The stored factor survives a
 * refusal: it was a real choice on a machine that accepted it, and the next
 * launch tries again.
 */
import { isTauriRuntime, setWebviewZoomFactor } from './tauri/webview';

/** 100% — the shipped size. */
export const WINDOW_ZOOM_DEFAULT = 1;
/** 200% — the enlargement the issue asks for. */
export const WINDOW_ZOOM_MAX = 2;
/** The offered steps, in percent. */
export const WINDOW_ZOOM_STEPS = [100, 125, 150, 175, 200] as const;

const STORAGE_KEY = 'buildmesh.window-zoom';

type ZoomListener = (percent: number) => void;
const listeners = new Set<ZoomListener>();

function loadWindowZoom(): number {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored !== null) {
      const percent = Number.parseInt(stored, 10);
      if (Number.isFinite(percent)) return clampPercent(percent);
    }
  } catch {
    // localStorage unavailable (test env, private-mode Safari before user
    // interaction) — fall through to the default.
  }
  return WINDOW_ZOOM_DEFAULT * 100;
}

function clampPercent(percent: number): number {
  if (!Number.isFinite(percent)) return WINDOW_ZOOM_DEFAULT * 100;
  const clamped = Math.min(WINDOW_ZOOM_MAX * 100, Math.max(WINDOW_ZOOM_DEFAULT * 100, percent));
  // Snap to the nearest offered step so the control and the stored value
  // never disagree about what "150%" means.
  return WINDOW_ZOOM_STEPS.reduce((best, step) =>
    Math.abs(step - clamped) < Math.abs(best - clamped) ? step : best,
  );
}

let _currentWindowZoom = loadWindowZoom();
/**
 * The zoom the native webview is really at. It starts at the shipped size
 * because a fresh webview does: the stored value is a request, and until the
 * platform honours it the window is still at 1x. It is the value a refusal
 * falls back to.
 */
let _appliedWindowZoom = WINDOW_ZOOM_DEFAULT * 100;

/** The active window zoom, as a percentage (100 = 1×). */
export function currentWindowZoom(): number {
  return _currentWindowZoom;
}

/** Subscribe to window-zoom changes. Not called on registration. */
export function onWindowZoomChange(cb: ZoomListener): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

/**
 * Apply a zoom factor to the native webview.
 *
 * Rejects when there is no webview to zoom (the browser dev server, the
 * mock IPC) or when the platform does not implement the command (Android).
 * The caller decides what to tell the user; this function never pretends a
 * zoom it could not perform happened.
 */
export async function applyWindowZoom(percent: number): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error('Window zoom needs the native app; it is not available in the browser.');
  }
  await setWebviewZoomFactor(percent / 100);
}

/**
 * Set the window zoom: apply it natively, then publish and persist it.
 *
 * The native call comes FIRST. A zoom the platform refused is never
 * published, so the control can never show a size the window is not at, and
 * the stored factor only ever records a zoom that was really applied. The
 * rejection is re-thrown so the caller can report it.
 */
export async function setWindowZoom(percent: number): Promise<void> {
  const next = clampPercent(percent);
  await applyWindowZoom(next);
  const changed = next !== _currentWindowZoom;
  _appliedWindowZoom = next;
  _currentWindowZoom = next;
  writeZoom(next);
  if (changed) listeners.forEach((cb) => cb(next));
}

/**
 * Apply the persisted window zoom at boot. Called once from main.tsx.
 *
 * The stored zoom is applied EVEN at the shipped 100%: WebView2's zoom factor
 * belongs to the controller, so a reloaded webview can still be zoomed from an
 * earlier session. Applying the stored value puts the window where the
 * control says it is, which is the whole point of the invariant. A browser-mode
 * boot has no webview to zoom, so it skips the call and stays quiet.
 *
 * Failures are logged, not thrown: a boot must not fail because the platform
 * refused the zoom. The control then shows the zoom the window really has —
 * the shipped size — while the stored factor is KEPT, because it was a real
 * choice on a machine that accepted it and the next launch tries it again.
 */
export async function applyWindowZoomAtBoot(): Promise<void> {
  if (!isTauriRuntime()) return;
  try {
    await applyWindowZoom(_currentWindowZoom);
    _appliedWindowZoom = _currentWindowZoom;
  } catch (err) {
    console.warn('[webviewZoom] Could not apply the persisted window zoom:', err);
    // The window is at the shipped size, so the control must not claim the
    // stored zoom. The stored factor is KEPT: it was a real choice on a
    // machine that accepted it, and the next launch tries it again.
    _currentWindowZoom = _appliedWindowZoom;
    listeners.forEach((cb) => cb(_currentWindowZoom));
  }
}

function writeZoom(percent: number): void {
  try {
    if (percent === WINDOW_ZOOM_DEFAULT * 100) {
      localStorage.removeItem(STORAGE_KEY);
    } else {
      localStorage.setItem(STORAGE_KEY, String(percent));
    }
  } catch {
    // localStorage unavailable — the in-memory value stays correct.
  }
}
