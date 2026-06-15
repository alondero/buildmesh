import { describe, it, expect } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';

/**
 * IPC seam guard (ADR-0010). Every Tauri `invoke` must go through the typed
 * wrapper in `src/lib/tauri.ts`, so a renamed `#[command]` breaks in one place
 * instead of silently across N components (the project's #1 runtime-failure
 * mode). This is the enforcement the repo uses in place of an ESLint rule (it
 * has no ESLint setup): a drift test with a (now-steady-state) allowlist.
 *
 * The allowlist started as the migration to-do list (#385) and shrank as the
 * sweep proceeded; it has reached steady state with a single legitimate
 * exception (see `EXEMPT_LEGITIMATE` below). Any new file importing raw
 * `invoke` fails this test immediately. Nothing outside `tauri.ts` and the
 * exempt list may import `invoke` from `@tauri-apps/api/core`.
 *
 * The wrapper itself is also the central IPC error-logging chokepoint
 * (issue #386) — see `_invoke` in `src/lib/tauri.ts` and the shape serializer
 * in `src/lib/ipcShape.ts`. That follow-up is the reason `frontendLog.ts` is
 * pinned as a peer-of-wrapper exemption below: the wrapper calls
 * `frontendLog` on every rejection, so routing `frontendLog.ts` through the
 * wrapper would risk a logging cycle.
 */

/**
 * The one legitimate, deliberate exception. `frontendLog.ts` is a peer of the
 * wrapper, not a consumer: it forwards console errors to the backend, and the
 * wrapper itself may eventually call *it* for central IPC error logging (per
 * ADR-0010's "deliberate follow-up"). Routing `frontendLog.ts` through the
 * wrapper would risk a logging cycle, so it stays a raw `invoke` site and
 * keeps its own re-entrancy guard. Documented at the file head too.
 */
const EXEMPT_LEGITIMATE = new Set<string>([
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
  it('only the wrapper and the documented exemption import raw invoke', () => {
    const importers = tsFiles(SRC_DIR)
      .filter((f) => IMPORTS_INVOKE.test(readFileSync(f, 'utf8')))
      .map(rel)
      .filter((f) => f !== WRAPPER);

    const unexpected = importers.filter((f) => !EXEMPT_LEGITIMATE.has(f));
    expect(
      unexpected,
      `These files import raw invoke. Route them through ` +
        `src/lib/tauri.ts (import * as api from '../lib/tauri'). See ADR-0010. ` +
        `The exemption list is closed — adding to it requires an ADR update.`,
    ).toEqual([]);
  });

  it('every exempt entry still imports raw invoke (no stale exemptions)', () => {
    const importers = new Set(
      tsFiles(SRC_DIR)
        .filter((f) => IMPORTS_INVOKE.test(readFileSync(f, 'utf8')))
        .map(rel),
    );
    const stale = [...EXEMPT_LEGITIMATE].filter((f) => !importers.has(f));
    expect(
      stale,
      'These exempt entries no longer import raw invoke — delete them so the ' +
        'exemption list stays a true map of legitimate exceptions.',
    ).toEqual([]);
  });
});
