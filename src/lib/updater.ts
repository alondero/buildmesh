import { check, type Update } from '@tauri-apps/plugin-updater';
import { getAppIdentifier } from './tauri';

// Updater state machine (issue #1526).
//
// The previous `runUpdateCheck` swallowed check failures as `null` and the
// install path called `relaunch()` directly, bypassing the Exit Readiness
// seam from #1501. This file replaces both: a typed result that
// distinguishes "up to date" from "feed unreachable", a granular install
// flow with progress events, and a sanitized error surface so the UI
// never shows raw URLs, tokens, or paths from the feed.

export interface UpdateSummary {
  version: string;
  /** Release notes (the GitHub Release body), trimmed. May be empty. */
  notes: string;
  /** Short headline shown in the prompt. */
  message: string;
}

/** Bytes transferred so far and, when known, the total payload size. The
 *  updater plugin emits `contentLength` as `null` for chunked / unknown-
 *  length responses — render an indeterminate bar in that case rather
 *  than fabricating a denominator. */
export interface DownloadProgress {
  downloaded: number;
  total: number | null;
}

/** Pure decision: should the updater run? All three inputs are pre-computed
 *  by the caller so tests can exercise every branch without stubbing
 *  `import.meta.env.PROD` (which Vite freezes at build time). */
export function decideUpdateEnabled(
  prod: boolean,
  hasTauriInternals: boolean,
  identifier: string | null,
): boolean {
  if (!prod) return false;
  if (!hasTauriInternals) return false;
  if (identifier === null) return false;
  if (isDevProfile(identifier)) return false;
  return true;
}

// The dev profile's bundle identifier is `com.alond.buildmesh.dev` — a single
// `endsWith` check is enough to distinguish it from the stable `com.alond.buildmesh`
// (set in `tauri.dev.conf.json` / `tauri.conf.json` respectively). Pure so
// the test mocks `getAppIdentifier` and asserts the guard.
export function isDevProfile(identifier: string): boolean {
  return identifier.endsWith('.dev');
}

// Pure — no plugin/IPC calls — so it's trivially unit-testable. Takes only the
// fields we render, not the full native `Update` handle.
export function describeUpdate(update: Pick<Update, 'version' | 'body'>): UpdateSummary {
  const version = update.version;
  const notes = (update.body ?? '').trim();
  return {
    version,
    notes,
    message: `Buildmesh ${version} is available.`,
  };
}

// Fetches and caches the running app's bundle identifier. Returns `null`
// outside Tauri (vite browser dev, tests without the mock) so the guard
// becomes a clean no-op rather than a thrown promise.
let _identifierCache: string | null | undefined; // undefined = not yet fetched
async function fetchIdentifier(): Promise<string | null> {
  if (_identifierCache !== undefined) return _identifierCache;
  if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) {
    _identifierCache = null;
    return null;
  }
  try {
    _identifierCache = await getAppIdentifier();
  } catch (e) {
    console.error('[updater] get_app_identifier failed:', e);
    _identifierCache = null;
  }
  return _identifierCache;
}

// Only run the updater inside a real Tauri production build AND only for
// the stable profile. Three guards: (1) `import.meta.env.PROD` rules out
// the vite browser dev server; (2) `__TAURI_INTERNALS__` rules out
// non-Tauri page loads; (3) the dev-profile check rules out the
// `tauri:build:dev` build (which is also a production-mode Vite build, so
// guard #1 alone can't tell it apart — see ADR 0021).
export async function updaterEnabled(): Promise<boolean> {
  const hasTauriInternals =
    typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  const identifier = hasTauriInternals ? await fetchIdentifier() : null;
  return decideUpdateEnabled(import.meta.env.PROD, hasTauriInternals, identifier);
}

/** Typed result of a check (issue #1526).
 *
 *  - `available` — feed has a newer version than the running build.
 *  - `current`   — running build is up to date.
 *  - `unreachable` — feed could not be read (offline, DNS, HTTP error,
 *                   bad signature, …). Surfaces an error to manual checks
 *                   only; quiet auto-checks suppress it.
 *  - `disabled`  — updater is not active in this build (non-prod,
 *                   non-Tauri page load, or dev profile).
 *
 *  Never throws — a failed check is a typed phase, not an exception
 *  callers must catch. */
export type CheckResult =
  | { phase: 'available'; update: Update; summary: UpdateSummary }
  | { phase: 'current' }
  | { phase: 'unreachable'; error: string }
  | { phase: 'disabled' };

// Strip URLs, tokens, and absolute paths from a thrown error's message
// before it reaches the UI. The Tauri updater plugin embeds the feed URL
// in `UpdaterError` messages — handing that straight to a toast would
// leak the release endpoint. Also collapses surrounding whitespace so a
// 200-char stacktrace doesn't blow out the modal's body.
const URL_RE = /\bhttps?:\/\/[^\s)]+/gi;
// Two token patterns — applied sequentially (BEARER first). The
// alternation-in-one-regex approach fails on `Authorization: Bearer
// <token>` because the `Authorization:` branch greedily consumes the
// `Bearer` keyword as part of the header value, leaving the actual
// token unreplaced. Splitting them and re-running lets the BEARER
// regex find its match on the original input before the AUTH regex
// scrubs the whole header.
const BEARER_RES = /\b(?:bearer|token|basic)\s+[^\s,)]+/gi;
const AUTH_RES = /\bauthorization\s*[:=]\s*[^\s,)]+/gi;
// Absolute paths. Windows paths (`C:\…` and UNC `\\server\share\…`)
// and POSIX sensitive roots (`/home`, `/Users`, `/var`, `/tmp`,
// `/etc`) can legitimately contain spaces — `C:\Users\Jane Doe\AppData\…`
// and `C:\Program Files\Buildmesh\…` are real cases the previous
// `[^\s)]+` silently leaked (surname + trailing path preserved in the
// modal). Stop at `,`, `)`, and `\n` (real error-message delimiters)
// but allow spaces inside the match; require a non-space, non-comma,
// non-backslash terminator so we don't capture trailing punctuation
// the error message appended. The reviewer flagged this as a privacy
// leak (issue #1526 follow-up).
const ABS_PATH_RES = /(?:[A-Za-z]:[\\/]|\\\\|\/(?:home|Users|var|tmp|etc)\/)(?:[^,\n]*[^\s,\n\\])/gi;

export function sanitizeUpdaterError(raw: unknown): string {
  let text = '';
  if (raw instanceof Error) text = raw.message;
  else if (typeof raw === 'string') text = raw;
  else if (raw && typeof raw === 'object' && 'message' in raw) {
    text = String((raw as { message: unknown }).message);
  } else {
    // Non-Error throws (undefined, numbers, plain objects) — nothing
    // meaningful to display. Stable fallback keeps the modal body
    // consistent rather than showing 'null' or '[object Object]'.
    return 'Unknown error.';
  }
  return text
    .replace(URL_RE, '<feed>')
    // BEARER first so the actual token value (not just the header) is
    // redacted; AUTH then sweeps any remaining auth-header value.
    .replace(BEARER_RES, '<redacted>')
    .replace(AUTH_RES, '<redacted>')
    .replace(ABS_PATH_RES, '<path>')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 240) || 'Unknown error.';
}

/** Probe the updater feed. Returns a typed `CheckResult` — never throws
 *  and never silently maps a failure to "no update" (the bug #1526
 *  caught). Quiet and manual callers share this function; the caller
 *  decides whether to surface `current` / `unreachable` to the user. */
export async function runUpdateCheck(): Promise<CheckResult> {
  if (!(await updaterEnabled())) return { phase: 'disabled' };
  try {
    const update = await check();
    if (!update) return { phase: 'current' };
    return {
      phase: 'available',
      update,
      summary: describeUpdate(update),
    };
  } catch (e) {
    console.error('[updater] check failed:', e);
    return { phase: 'unreachable', error: sanitizeUpdaterError(e) };
  }
}

/** Download the staged update, calling `onProgress` with cumulative
 *  byte counts as the plugin emits per-chunk events. Resolves once
 *  the response stream ends. Rejects on plugin failure with a
 *  sanitized error so the UI never sees raw URLs / paths / tokens.
 *  The native `Update` handle is NOT modified — callers can pass it
 *  to `installUpdate` next, or keep it on the `failed` phase so a
 *  retry resumes the staged binary (the Tauri updater keeps partial
 *  downloads on disk and re-validates on resume). */
export async function downloadUpdate(
  update: Update,
  onProgress?: (progress: DownloadProgress) => void,
): Promise<void> {
  // The Tauri updater plugin emits a discriminated union of download
  // events: `Started` carries the total length (may be undefined for
  // chunked responses), `Progress` carries the per-chunk byte count,
  // `Finished` is the terminal event. Track the running total in this
  // closure — survives across the awaited promise because `download`
  // resolves only after the response stream ends.
  let downloaded = 0;
  let total: number | null = null;
  try {
    await update.download((event) => {
      if (event.event === 'Started') {
        if (typeof event.data.contentLength === 'number') {
          total = event.data.contentLength;
        }
        onProgress?.({ downloaded, total });
        return;
      }
      if (event.event === 'Progress') {
        downloaded += event.data.chunkLength;
        onProgress?.({ downloaded, total });
        return;
      }
      // 'Finished' has no data; final tick with the running totals so
      // the UI can show a complete bar before transitioning to
      // `installing`.
      onProgress?.({ downloaded, total });
    });
  } catch (e) {
    console.error('[updater] download failed:', e);
    throw new Error(sanitizeUpdaterError(e));
  }
}

/** Install the staged update. The plugin stages the binary on top of
 *  the running process; `relaunch()` (or app restart) picks it up.
 *  Rejects on plugin failure with a sanitized error. Callers should
 *  transition to `installing` BEFORE invoking this so the UI's
 *  progress surface matches the actual operation. */
export async function installUpdate(update: Update): Promise<void> {
  try {
    await update.install();
  } catch (e) {
    console.error('[updater] install failed:', e);
    throw new Error(sanitizeUpdaterError(e));
  }
}

// Exported for tests so the module-level identifier cache can be reset
// between cases.
export function __resetIdentifierCacheForTests(): void {
  _identifierCache = undefined;
}