/**
 * Window zoom — the 200% enlargement path (issue #2739).
 *
 * The interface text scale tops out at 150% because the dense chrome clips
 * past that. The window zoom is the supported way to go further: it is the
 * platform's own zoom (`plugin:webview|set_webview_zoom`), so chrome, labels
 * and content all grow together and nothing has to be re-flowed to fit.
 *
 * Pins three contracts:
 *
 *   1. The chosen factor reaches the native webview, and 200% really is 2.0.
 *   2. The choice survives a relaunch and is re-applied at boot.
 *   3. A platform that cannot zoom says so instead of pretending the zoom
 *      happened — a silent no-op would leave the user believing the window
 *      is at 200% when it is not.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

const STORAGE_KEY = 'buildmesh.window-zoom';

const setWebviewZoomFactor = vi.fn(async () => {});
const isTauriRuntime = vi.fn(() => true);

vi.mock('../../src/lib/tauri/webview', () => ({
  isTauriRuntime: () => isTauriRuntime(),
  setWebviewZoomFactor: (factor: number) => setWebviewZoomFactor(factor),
}));

beforeEach(() => {
  localStorage.clear();
  setWebviewZoomFactor.mockClear();
  setWebviewZoomFactor.mockImplementation(async () => {});
  isTauriRuntime.mockClear();
  isTauriRuntime.mockImplementation(() => true);
  vi.resetModules();
});

afterEach(() => {
  localStorage.clear();
});

describe('webviewZoom.ts — the native call', () => {
  it('applies 200% as a zoom factor of 2', async () => {
    const { setWindowZoom } = await import('../../src/lib/webviewZoom');
    await setWindowZoom(200);
    expect(setWebviewZoomFactor).toHaveBeenCalledWith(2);
  });

  it('applies every offered step', async () => {
    const { setWindowZoom, WINDOW_ZOOM_STEPS } = await import('../../src/lib/webviewZoom');
    expect(WINDOW_ZOOM_STEPS).toContain(200);
    for (const step of WINDOW_ZOOM_STEPS) {
      setWebviewZoomFactor.mockClear();
      await setWindowZoom(step);
      expect(setWebviewZoomFactor).toHaveBeenCalledWith(step / 100);
    }
  });

  it('snaps an off-step request to the nearest offered step', async () => {
    const { setWindowZoom, currentWindowZoom } = await import('../../src/lib/webviewZoom');
    await setWindowZoom(160);
    expect(currentWindowZoom()).toBe(150);
    expect(setWebviewZoomFactor).toHaveBeenCalledWith(1.5);
  });

  it('rejects outside the native runtime instead of pretending to zoom', async () => {
    isTauriRuntime.mockImplementation(() => false);
    const { setWindowZoom } = await import('../../src/lib/webviewZoom');
    await expect(setWindowZoom(200)).rejects.toThrow(/native app/i);
    expect(setWebviewZoomFactor).not.toHaveBeenCalled();
  });

  it('puts the control back where the truth is when the platform refuses the zoom', async () => {
    setWebviewZoomFactor.mockImplementation(async () => {
      throw new Error('not supported on this platform');
    });
    const { setWindowZoom, currentWindowZoom } = await import('../../src/lib/webviewZoom');
    await expect(setWindowZoom(200)).rejects.toThrow(/not supported/);
    // The window is still at 100%, so the radio must not keep claiming 200%
    // and nothing may be stored that the window is not at.
    expect(currentWindowZoom()).toBe(100);
    expect(localStorage.getItem(STORAGE_KEY)).toBeNull();
  });

  it('never publishes a zoom the platform refused', async () => {
    setWebviewZoomFactor.mockImplementation(async () => {
      throw new Error('not supported on this platform');
    });
    const { setWindowZoom, onWindowZoomChange } = await import('../../src/lib/webviewZoom');
    const seen: number[] = [];
    const unsubscribe = onWindowZoomChange((percent) => seen.push(percent));
    await expect(setWindowZoom(200)).rejects.toThrow(/not supported/);
    unsubscribe();
    // The control is driven by this subscription, so it must never hear a
    // zoom the window does not have.
    expect(seen).toEqual([]);
  });

  it('keeps a previously accepted zoom when a later one is refused', async () => {
    const { setWindowZoom, currentWindowZoom } = await import('../../src/lib/webviewZoom');
    await setWindowZoom(150);
    expect(localStorage.getItem(STORAGE_KEY)).toBe('150');

    setWebviewZoomFactor.mockImplementation(async () => {
      throw new Error('not supported on this platform');
    });
    await expect(setWindowZoom(200)).rejects.toThrow(/not supported/);
    // The refusal does not discard a choice the platform had already
    // accepted — the next launch tries again.
    expect(currentWindowZoom()).toBe(150);
    expect(localStorage.getItem(STORAGE_KEY)).toBe('150');
  });
});

describe('webviewZoom.ts — persistence', () => {
  it('persists a non-default zoom and removes the key on the way back to 100%', async () => {
    const { setWindowZoom } = await import('../../src/lib/webviewZoom');
    await setWindowZoom(200);
    expect(localStorage.getItem(STORAGE_KEY)).toBe('200');
    await setWindowZoom(100);
    expect(localStorage.getItem(STORAGE_KEY)).toBeNull();
  });

  it('reads a stored percentage at module load', async () => {
    localStorage.setItem(STORAGE_KEY, '150');
    const { currentWindowZoom } = await import('../../src/lib/webviewZoom');
    expect(currentWindowZoom()).toBe(150);
  });

  it('re-applies the persisted zoom at boot', async () => {
    localStorage.setItem(STORAGE_KEY, '200');
    const { applyWindowZoomAtBoot } = await import('../../src/lib/webviewZoom');
    await applyWindowZoomAtBoot();
    expect(setWebviewZoomFactor).toHaveBeenCalledWith(2);
  });

  it('puts the webview at the stored zoom at boot, including the shipped size', async () => {
    // WebView2's zoom factor belongs to the controller, so a reloaded webview
    // can still be zoomed from an earlier session. Applying the stored value
    // puts the window where the control says it is.
    const { applyWindowZoomAtBoot } = await import('../../src/lib/webviewZoom');
    await applyWindowZoomAtBoot();
    expect(setWebviewZoomFactor).toHaveBeenCalledWith(1);
  });

  it('does not touch the webview outside the native runtime', async () => {
    // A browser-mode boot has no webview to zoom, and it must stay quiet.
    isTauriRuntime.mockImplementation(() => false);
    const { applyWindowZoomAtBoot } = await import('../../src/lib/webviewZoom');
    await applyWindowZoomAtBoot();
    expect(setWebviewZoomFactor).not.toHaveBeenCalled();
  });

  it('a failed boot zoom never breaks startup and falls back to the shipped size', async () => {
    setWebviewZoomFactor.mockImplementation(async () => {
      throw new Error('boom');
    });
    localStorage.setItem(STORAGE_KEY, '200');
    const { applyWindowZoomAtBoot, currentWindowZoom } = await import('../../src/lib/webviewZoom');
    await expect(applyWindowZoomAtBoot()).resolves.toBeUndefined();
    // The window is at 100%, so the control must not claim 200%.
    expect(currentWindowZoom()).toBe(100);
    // The stored factor is kept: it was a real choice on a machine that
    // accepted it, and the next launch tries again.
    expect(localStorage.getItem(STORAGE_KEY)).toBe('200');
  });
});
