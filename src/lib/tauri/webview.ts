/**
 * Webview IPC facet — issue #2739.
 *
 * The one Tauri call Buildmesh makes about the webview itself rather than
 * about a domain: setting the native zoom factor. On Windows this lands on
 * WebView2's `ICoreWebView2Controller::SetZoomFactor`, so the whole window —
 * chrome, labels and content — is scaled by the platform, not by a CSS
 * rule that would have to be chased through every surface.
 *
 * `plugin:webview|set_webview_zoom` is NOT part of `core:webview:default`;
 * it needs `core:webview:allow-set-webview-zoom` in
 * `src-tauri/capabilities/default.json`. The command is desktop-only —
 * Tauri's own docs say "Android: not supported, macOS: macOS 11+, iOS:
 * iOS 14+" — so every caller must be able to cope with a rejection.
 */
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { isTauri } from '@tauri-apps/api/core';

/**
 * True when the frontend is running inside a Tauri webview.
 *
 * The browser dev server and the mock IPC used by the UI checks have no
 * webview to zoom, so the Settings control has to be able to tell the user
 * that the window zoom is unavailable instead of failing on click.
 */
export function isTauriRuntime(): boolean {
  try {
    return isTauri();
  } catch {
    return false;
  }
}

/**
 * Set the native webview zoom factor. `1` is 100%.
 *
 * Rejects when the platform does not implement the command (Android) or
 * when the capability is missing — the caller surfaces that, it does not
 * swallow it, because a silent no-op would leave the user believing the
 * window is at 200% when it is not.
 */
export async function setWebviewZoomFactor(scaleFactor: number): Promise<void> {
  await getCurrentWebview().setZoom(scaleFactor);
}
