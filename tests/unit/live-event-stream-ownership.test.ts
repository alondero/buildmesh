/**
 * One subscriber per live event stream (issue #2531).
 *
 * The refactor's whole point is that a live event has ONE owner: the
 * module that subscribes to it, converts its payload and publishes typed
 * updates. This test is the structural guard for that property — it
 * walks `src/` and asserts that each event name reaches the backend
 * event bus from exactly one file.
 *
 * Without it, "add a listener for `circuit-run-updated` in a component"
 * is a one-line change that reintroduces the exact duplication this
 * issue removed.
 *
 * Scope: the Tauri `listen` transport. The mobile client has its own
 * WebSocket transport (`src/mobile/useWsEvents.ts`) with its own single
 * dispatcher — a different transport, not a second subscriber to the
 * Tauri event.
 */
import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

const REPO_ROOT = resolve(__dirname, '..', '..');
const FRONTEND_SRC = join(REPO_ROOT, 'src');

function walkTsFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) walkTsFiles(full, out);
    else if (/\.(ts|tsx)$/.test(entry)) out.push(full);
  }
  return out;
}

/** Source with comments stripped — a `listen` in a comment is not a call. */
function codeOf(file: string): string {
  return readFileSync(file, 'utf8')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/\/\/.*$/gm, '');
}

/**
 * Every `listen` call in the tree, with the event name it names.
 *
 * The event name may be a string literal or a constant; constants are
 * resolved against every `const NAME = 'literal'` declaration in `src/`,
 * so `listen(PROVIDER_LIST_CHANGED_EVENT, ...)` is found as surely as
 * `listen('provider-list-changed', ...)`.
 */
function listenCallSites(): Array<{ event: string; file: string }> {
  const files = walkTsFiles(FRONTEND_SRC);
  const constants = new Map<string, string>();
  for (const file of files) {
    for (const m of codeOf(file).matchAll(/(?:export\s+)?const\s+([A-Za-z_$][\w$]*)\s*=\s*['"]([^'"]+)['"]/g)) {
      constants.set(m[1], m[2]);
    }
  }

  const sites: Array<{ event: string; file: string }> = [];
  for (const file of files) {
    const code = codeOf(file);
    // The payload type is a simple identifier or a `|`-union — never
    // nested angle brackets — so `<[^<>]*>` is enough.
    const re = /\blisten\s*(?:<[^<>]*>)?\s*\(\s*(?:(['"])([a-zA-Z][a-zA-Z0-9_-]*)\1|([A-Za-z_$][\w$]*))/g;
    for (const m of code.matchAll(re)) {
      const event = m[2] ?? (m[3] ? constants.get(m[3]) : undefined);
      if (event === undefined) continue;
      sites.push({ event, file: relative(REPO_ROOT, file).replace(/\\/g, '/') });
    }
  }
  return sites;
}

const SITES = listenCallSites();

/** The files whose `listen` calls name `event`. */
function subscribersOf(event: string): string[] {
  return [...new Set(SITES.filter((site) => site.event === event).map((site) => site.file))].sort();
}

describe('live event stream ownership (issue #2531)', () => {
  it.each([
    ['circuit-run-updated', 'src/lib/streams/circuitRunStream.ts'],
    ['provider-list-changed', 'src/lib/streams/providerListStream.ts'],
    ['worktree-directory-changed', 'src/lib/streams/worktreeDirectoryStream.ts'],
    ['mesh-sync-warning', 'src/lib/streams/meshSyncWarningStream.ts'],
  ])('%s has exactly one subscriber — its owner', (event, owner) => {
    // Six modules used to subscribe to `circuit-run-updated` and each one
    // re-implemented the "is this about MY run?" filter. The other three
    // had two subscribers each. One owner per stream, no exceptions.
    expect(subscribersOf(event)).toEqual([owner]);
  });

  it.each([
    'circuit-pr-ready',
    'attention-cleared',
    'agent-lifecycle',
    'node-renamed',
    'node-created',
    'node-deleted',
    'node-activated',
    'node-spawn-completed',
    'node-spawn-failed',
  ])('%s still has exactly one subscriber — the agent-node listener module', (event) => {
    // The node-lifecycle events were already single-owner. Moving
    // `circuit-run-updated` out must not have disturbed them.
    expect(subscribersOf(event)).toEqual(['src/stores/agentNodeListeners.ts']);
  });

  it('finds the streams it claims to guard', () => {
    // A silent scan failure would make every assertion above vacuous.
    expect(SITES.length).toBeGreaterThanOrEqual(15);
    expect(subscribersOf('circuit-run-updated')).not.toEqual([]);
  });
});
