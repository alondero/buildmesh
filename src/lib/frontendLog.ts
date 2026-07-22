/**
 * Frontend log bridge.
 *
 * Forwards `console.error`, `console.warn`, `console.info`, `window.error`,
 * and `unhandledrejection` into the Rust `log_frontend` Tauri command so they
 * land in `buildmesh.log` alongside backend traces.
 *
 * Why: WebView2's devtools console is invisible to anyone debugging headlessly
 * (CI, `/verify`, autonomous agents). Past silent regressions
 * (e.g. `requestAnimationFrame` "Illegal invocation" swallowed by a Tauri
 * listener) only surface here if we forward them out of the webview.
 *
 * `console.info` was added in issue #602 so the frontend's `xterm_mount`
 * spawn-timing checkpoint (emitted by `TerminalRegistry.attachToDOM`) lands
 * in `buildmesh.log` next to the Rust `SpawnTimer` lines — letting a reader
 * `grep spawn_timing:` across both halves of the IPC boundary as one
 * timeline. Without this forwarding, the checkpoint stays devtools-only and
 * the cross-boundary format parity is silent for headless debuggers.
 *
 * Design notes:
 * - Best-effort. If `invoke` fails (e.g. command not registered, app shutting
 *   down) we silently swallow — we cannot afford to log the log failure.
 * - Re-entrancy guarded: if Tauri's invoke path itself calls console.error
 *   while we're mid-forward, we drop the recursive event.
 * - The original console functions still run, so devtools output is unchanged.
 *
 * **IPC-seam exemption (ADR-0010):** this file deliberately calls
 * `@tauri-apps/api/core` `invoke` directly rather than routing through
 * `src/lib/tauri.ts`. It is a *peer* of the wrapper, not a consumer: the
 * wrapper may eventually call *it* for central IPC error logging (the
 * "deliberate follow-up" noted in ADR-0010). Routing through the wrapper
 * would risk a logging cycle. The seam guard in
 * `tests/unit/tauri-ipc-seam.test.ts` pins this exemption explicitly.
 */

import { invoke } from '@tauri-apps/api/core';

export type LogLevel = 'error' | 'warn' | 'info' | 'debug';

function serializeArg(arg: unknown): string {
  if (arg instanceof Error) {
    return `${arg.name}: ${arg.message}\n${arg.stack ?? '<no stack>'}`;
  }
  if (typeof arg === 'string') return arg;
  if (arg === null || arg === undefined) return String(arg);
  try {
    return JSON.stringify(arg);
  } catch {
    return String(arg);
  }
}

function format(args: unknown[]): string {
  return args.map(serializeArg).join(' ');
}

let suppressForward = 0;
let installed = false;

function forward(level: LogLevel, message: string): void {
  if (suppressForward > 0) return;
  suppressForward++;
  try {
    void invoke('log_frontend', { level, message }).catch(() => {
      // Intentional: a failure here cannot itself be logged without
      // recursing. The original console.* call has already produced a
      // devtools entry.
    });
  } finally {
    suppressForward--;
  }
}

/**
 * Manually forward a message to the backend log without going through
 * console.*. Useful for ErrorBoundary-style reporters that want to log
 * structured context rather than a free-form string.
 */
export function logFrontend(level: LogLevel, message: string): void {
  forward(level, message);
}

/**
 * Install the global hooks. Idempotent — calling it twice is a no-op.
 * Call once at startup.
 */
export function installFrontendLogBridge(): void {
  if (installed) return;
  installed = true;

  const origError = console.error.bind(console);
  const origWarn = console.warn.bind(console);
  const origInfo = console.info.bind(console);

  console.error = (...args: unknown[]) => {
    origError(...args);
    forward('error', format(args));
  };

  console.warn = (...args: unknown[]) => {
    origWarn(...args);
    forward('warn', format(args));
  };

  // Forward `console.info` so the `xterm_mount` spawn-timing checkpoint
  // (issue #602) reaches `buildmesh.log`. Same shape as the error/warn
  // patches above; the Rust side's `log_frontend` already maps
  // `level="info"` to `tracing::info!(target: "frontend", …)`.
  console.info = (...args: unknown[]) => {
    origInfo(...args);
    forward('info', format(args));
  };

  window.addEventListener('error', (e: ErrorEvent) => {
    const where = e.filename ? `${e.filename}:${e.lineno}:${e.colno}` : 'unknown';
    const detail = e.error instanceof Error
      ? serializeArg(e.error)
      : (e.message ?? 'unknown error');
    forward('error', `[window.error @ ${where}] ${detail}`);
  });

  window.addEventListener('unhandledrejection', (e: PromiseRejectionEvent) => {
    forward('error', `[unhandledrejection] ${serializeArg(e.reason)}`);
  });
}

// Test-only — reset internal state between tests.
export function _resetFrontendLogBridgeForTests(): void {
  installed = false;
  suppressForward = 0;
}
