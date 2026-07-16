import { check, type Update } from '@tauri-apps/plugin-updater';
import { getAppIdentifier } from './tauri';

// Auto-update plumbing (issue #826). The Tauri updater plugin does the heavy
// lifting (download, minisign verification, install); this module is a thin,
// testable seam on top of it so the React layer never touches the plugin API
// directly and the decision logic stays unit-testable.

export interface UpdateSummary {
  version: string;
  /** Release notes (the GitHub Release body), trimmed. May be empty. */
  notes: string;
  /** Short headline shown in the prompt. */
  message: string;
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

// The dev profile's bundle identifier is `com.alond.buildmesh.dev` — a single
// `endsWith` check is enough to distinguish it from the stable `com.alond.buildmesh`
// (set in `tauri.dev.conf.json` / `tauri.conf.json` respectively). Pure so
// the test mocks `getAppIdentifier` and asserts the guard.
export function isDevProfile(identifier: string): boolean {
  return identifier.endsWith('.dev');
}

// Pure decision: should the updater run? All three inputs are pre-computed
// by the caller so tests can exercise every branch without stubbing
// `import.meta.env.PROD` (which Vite freezes at build time).
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

// Resolves to the pending `Update` (with its native download/install handle)
// or null when up to date / disabled / unreachable. Never throws — a failed
// check (offline, feed down) is a non-event, not an error the user must see.
export async function runUpdateCheck(): Promise<Update | null> {
  if (!(await updaterEnabled())) return null;
  try {
    return await check();
  } catch (e) {
    console.error('[updater] check failed:', e);
    return null;
  }
}

// Exported for tests so the module-level identifier cache can be reset
// between cases.
export function __resetIdentifierCacheForTests(): void {
  _identifierCache = undefined;
}
