import { describe, it, expect } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';

/**
 * IPC seam guard (ADR-0010). Every Tauri `invoke` must go through the typed
 * wrapper in `src/lib/tauri.ts`, so a renamed `#[command]` breaks in one place
 * instead of silently across N components (the project's #1 runtime-failure
 * mode). This is the enforcement the repo uses in place of an ESLint rule (it
 * has no ESLint setup): a drift test with a shrinking allowlist.
 *
 * The allowlist is the set of files NOT YET migrated onto the wrapper — it IS
 * the remaining to-do list. As each file is migrated (raw `invoke` →
 * `import * as api from '../lib/tauri'`), delete it from the list here. A NEW
 * file importing raw `invoke` fails this test immediately. When the list is
 * empty, the seam is fully enforced: nothing outside `tauri.ts` may import
 * `invoke` from `@tauri-apps/api/core`.
 *
 * Stores were migrated first (tracer bullet); the rest follow.
 */

// Files still importing raw `invoke`, relative to repo root, forward-slashed.
// Shrink this as the component sweep proceeds; do not add to it.
const ALLOWLIST = new Set<string>([
  'src/App.tsx',
  'src/components/AppSettings/AppSettingsModal.tsx',
  'src/components/RemoteAccess/RemoteAccessModal.tsx',
  'src/components/Terminal/BuildRunTerminal.tsx',
  'src/components/Terminal/Terminal.tsx',
  'src/components/Terminal/TerminalRegistry.ts',
  'src/components/Terminal/handoverProviderCache.ts',
  'src/lib/fileDropPaste.ts',
  'src/lib/frontendLog.ts',
]);

// The one place `invoke` is allowed to be imported — the wrapper itself.
const WRAPPER = 'src/lib/tauri.ts';

const IMPORTS_INVOKE =
  /import\s*\{[^}]*\binvoke\b[^}]*\}\s*from\s*['"]@tauri-apps\/api\/core['"]/;

const SRC_DIR = join(process.cwd(), 'src');

function tsFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...tsFiles(full));
    } else if (/\.(ts|tsx)$/.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

function rel(full: string): string {
  return relative(process.cwd(), full).split(sep).join('/');
}

describe('Tauri IPC seam (ADR-0010)', () => {
  it('only allowlisted files (plus the wrapper) import raw invoke', () => {
    const importers = tsFiles(SRC_DIR)
      .filter((f) => IMPORTS_INVOKE.test(readFileSync(f, 'utf8')))
      .map(rel)
      .filter((f) => f !== WRAPPER);

    const unexpected = importers.filter((f) => !ALLOWLIST.has(f));
    expect(
      unexpected,
      `These files import raw invoke but are not allowlisted. Route them through ` +
        `src/lib/tauri.ts (import * as api from '../lib/tauri'). See ADR-0010.`,
    ).toEqual([]);
  });

  it('the allowlist has no stale entries (migrated files removed)', () => {
    const importers = new Set(
      tsFiles(SRC_DIR)
        .filter((f) => IMPORTS_INVOKE.test(readFileSync(f, 'utf8')))
        .map(rel),
    );
    const stale = [...ALLOWLIST].filter((f) => !importers.has(f));
    expect(
      stale,
      'These allowlist entries no longer import raw invoke — delete them from ' +
        'ALLOWLIST so the ratchet keeps tightening.',
    ).toEqual([]);
  });
});
