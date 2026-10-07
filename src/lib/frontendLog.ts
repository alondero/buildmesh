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

/** Per-argument cap. The Rust command caps again before the line is persisted. */
const ARG_CAP = 4096;
const STACK_CAP = 2048;
const STRING_CAP = 1024;
const MAX_DEPTH = 6;
const MAX_KEYS = 40;

/**
 * Same secret-word list as `SECRET_WORDS` in `src-tauri/src/secret_scrubber.rs`.
 * Bare `auth` is omitted so `author` survives. `ticket` covers pairing and
 * WebSocket handshake tickets. `pair` is omitted so `repair` survives; the
 * `#pair=` fragment is masked on the Rust side, which is the persistence boundary.
 */
const SECRET_KEY =
  /password|passwd|secret|token|api[_-]?key|access[_-]?key|client[_-]?secret|credentials?|auth[_-]?token|private[_-]?key|ticket/i;

/**
 * Free-text credential shapes, the same set as `TOKEN_RES`, `AUTH_SCHEME_RE`,
 * `PRIVATE_KEY_RE`, and `PAIRING_FRAGMENT_RE` in `secret_scrubber.rs`.
 * Structured keys are handled separately. These run on the whole argument
 * before the length cap: slicing first can leave a prefix of `sk-…` that the
 * Rust masker no longer recognizes.
 */
function redactFreeText(text: string): string {
  let out = text.replace(
    /-----BEGIN[A-Z ]*PRIVATE KEY-----[\s\S]*?-----END[A-Z ]*PRIVATE KEY-----/g,
    '[REDACTED PRIVATE KEY]',
  );
  out = out.replace(/#pair=[^\s&#]+/gi, '#pair=[REDACTED]');
  out = out.replace(
    /\b(Bearer|Basic|token|OAuth)\s+[A-Za-z0-9._\-+/]{8,}={0,2}/gi,
    '$1 [REDACTED]',
  );
  out = out.replace(/\bgh[opsur]_[A-Za-z0-9]{20,}\b/g, '[REDACTED]');
  out = out.replace(/\bgithub_pat_[A-Za-z0-9_]{20,}\b/g, '[REDACTED]');
  out = out.replace(/\bAKIA[0-9A-Z]{16}\b/g, '[REDACTED]');
  out = out.replace(/\bAIza[0-9A-Za-z_-]{35}\b/g, '[REDACTED]');
  out = out.replace(/\bxox[baprs]-[A-Za-z0-9-]{10,}\b/g, '[REDACTED]');
  out = out.replace(/\bsk-[A-Za-z0-9_-]{20,}\b/g, '[REDACTED]');
  return out;
}

function cap(text: string, max: number): string {
  const safe = redactFreeText(text);
  if (safe.length <= max) return safe;
  return `${safe.slice(0, max)}…<truncated ${safe.length - max} chars>`;
}

function isSecretKey(key: string): boolean {
  return SECRET_KEY.test(key);
}

/**
 * An `Error` is not `JSON.stringify`'d. `message` and `stack` are
 * non-enumerable, so stringifying the object drops the diagnostic and keeps
 * any extra field a caller attached — often the credential. Name, message,
 * and a capped stack are the diagnostic. A `cause` is walked with the same
 * rules as any other thrown value.
 */
function formatError(err: Error, depth: number): string {
  const rawMessage = String(err.message ?? '');
  const stack = cap(err.stack ?? '<no stack>', STACK_CAP);
  // A V8 `stack` already begins with `Name: message`. Printing the header and
  // then the stack that repeats it duplicates the diagnostic and spends the
  // stack cap on it, so drop the repeat when the stack carries one.
  const header = `${err.name}: ${rawMessage}`;
  const frames = stack.startsWith(header) ? stack.slice(header.length).replace(/^\n/, '') : stack;
  let text = `${err.name}: ${cap(rawMessage, STRING_CAP)}\n${frames}`;
  // `lib` does not include ES2022 `Error.cause`. Read it only when present.
  const cause = (err as Error & { cause?: unknown }).cause;
  if (depth < MAX_DEPTH && cause !== undefined) {
    text += `\ncaused by: ${serializeArg(cause, depth + 1)}`;
  }
  return text;
}

function boundValue(value: unknown, depth: number): unknown {
  if (typeof value === 'string') return cap(value, STRING_CAP);
  if (value === null || typeof value !== 'object') return value;
  if (value instanceof Error) return formatError(value, depth);
  if (depth >= MAX_DEPTH) return '…';
  if (Array.isArray(value)) {
    return value.slice(0, MAX_KEYS).map(item => boundValue(item, depth + 1));
  }
  const source = value as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(source).slice(0, MAX_KEYS)) {
    out[key] = isSecretKey(key) ? '[REDACTED]' : boundValue(source[key], depth + 1);
  }
  return out;
}

function serializeArg(arg: unknown, depth = 0): string {
  if (arg instanceof Error) return cap(formatError(arg, depth), ARG_CAP);
  if (typeof arg === 'string') return cap(arg, ARG_CAP);
  if (arg === null || arg === undefined) return String(arg);
  if (typeof arg !== 'object') return cap(String(arg), ARG_CAP);
  try {
    return cap(JSON.stringify(boundValue(arg, depth)) ?? String(arg), ARG_CAP);
  } catch {
    return cap(String(arg), ARG_CAP);
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
