import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, expect, it } from 'vitest';
import { startDevServer, stopDevServer } from '../../scripts/ui-shot-server.mjs';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const require = createRequire(import.meta.url);
const viteEntrypoint = resolve(dirname(require.resolve('vite/package.json')), 'bin', 'vite.js');

const children = new Set<any>();

async function freePort() {
  const server = createServer();
  await new Promise<void>((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolvePromise);
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Could not determine the free port');
  const port = address.port;
  await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
  return port;
}

/** A listener that holds a port and answers `respond` when asked. */
async function listening(respond?: (socket: any) => void) {
  const sockets = new Set<any>();
  const server = createServer((socket) => {
    sockets.add(socket);
    if (respond) respond(socket);
    else socket.on('data', () => {});
  });
  server.on('connection', (socket) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
  });
  await new Promise<void>((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolvePromise);
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Could not determine the free port');
  return {
    port: address.port,
    async close() {
      for (const socket of sockets) socket.destroy();
      await new Promise<void>((resolvePromise) => server.close(() => resolvePromise()));
    },
  };
}

afterEach(async () => {
  for (const child of children) await stopDevServer(child);
  children.clear();
});

/** Paths compared as forward-slash strings, folded to lower case where the file system is. */
function comparable(value: string) {
  const normalized = value.replaceAll('\\', '/').replace(/\/+$/, '');
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

/** Whether `child` sits inside `parent`. */
function isInside(child: string, parent: string) {
  const from = comparable(parent);
  const to = comparable(child);
  return to === from || to.startsWith(`${from}/`);
}

/**
 * The directory a server serves its optimized dependencies from, read from the
 * URLs it hands the browser. A served URL names the cache directory, so this
 * observes where the server really put its optimizer output.
 */
async function dependencyDirectory(url: string, root = repoRoot) {
  const entry = await fetch(`${url}/src/main.tsx`);
  const text = await entry.text();
  const match = text.match(/["']([^"']+\/deps\/[^"']+)["']/);
  if (!match) throw new Error(`the server at ${url} served no optimized dependency URL`);
  const pathname = decodeURIComponent(new URL(match[1], url).pathname);
  // `/@fs/C:/checkout/.tmp/vite/deps/react.js` names an absolute path;
  // `/.tmp/vite/deps/react.js` is relative to the server's root.
  const absolute = pathname.startsWith('/@fs/')
    ? pathname.slice(4).replace(/^\/([a-zA-Z]:)/, '$1')
    : resolve(root, `.${pathname}`);
  const normalized = absolute.replaceAll('\\', '/');
  const marker = normalized.toLowerCase().indexOf('/deps/');
  if (marker < 0) throw new Error(`unexpected dependency URL: ${pathname}`);
  return normalized.slice(0, marker);
}

/**
 * The optimized dependency graph a browser fetches: the entry's shims and the
 * bundles those shims re-export from.
 */
async function dependencyGraph(url: string) {
  const entry = await fetch(`${url}/src/main.tsx`);
  const text = await entry.text();
  const shims = [...text.matchAll(/from\s+["']([^"']+\/deps\/[^"']+)["']/g)]
    .map((match) => new URL(match[1], url));
  const bundles: URL[] = [];
  for (const shim of shims) {
    const body = await (await fetch(shim)).text();
    for (const match of body.matchAll(/from\s+["']([^"']+\/deps\/[^"']+)["']/g)) {
      bundles.push(new URL(match[1], shim));
    }
  }
  return { shims, bundles };
}

/**
 * Start this worktree's own Vite server, the way `npm run dev` does: this
 * checkout's configuration and no private cache directory.
 */
async function startWorktreeDevServer(url: string, timeoutMs = 120000) {
  const env = { ...process.env };
  delete env.BUILDMESH_VITE_CACHE_DIR;
  const child = spawn(
    process.execPath,
    [viteEntrypoint, '--host', '127.0.0.1', '--port', new URL(url).port],
    { cwd: repoRoot, env, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  children.add(child);
  // Drain the child's pipes: a full pipe buffer would block the server, and
  // the test would wait for a server that had stopped making progress.
  child.stdout?.resume();
  child.stderr?.resume();
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return child;
    } catch { /* the server is not answering yet */ }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 200));
  }
  throw new Error(`the worktree dev server did not answer at ${url} within ${timeoutMs}ms`);
}

describe('ui-shot Vite server', () => {
  it("keeps each server's dependency cache inside this worktree and out of a shared node_modules", async () => {
    // The optimizer cache is the on-disk state two Vite servers can share.
    // Vite removes `<cacheDir>/deps` when it starts with a config hash that
    // differs from the cached one, and that hash covers the absolute root, so
    // servers in different worktrees compute different hashes. With Vite's
    // default `node_modules/.vite` — one directory when the worktrees share a
    // node_modules junction — the second server removed the dependency graph
    // the first one was serving, and the browser got `504 (Outdated Optimize
    // Dep)` instead of the app (#2515). The served dependency URLs name the
    // directory each server used, so this observes the layout instead of
    // reading the configuration text.
    const port = await freePort();
    const url = `http://127.0.0.1:${port}`;
    const worktree = await startWorktreeDevServer(url);

    const uiShotPort = await freePort();
    const uiShotUrl = `http://127.0.0.1:${uiShotPort}`;
    const uiShot = await startDevServer(uiShotUrl);
    expect(uiShot).not.toBeNull();
    children.add(uiShot);

    const worktreeCache = await dependencyDirectory(url);
    const uiShotCache = await dependencyDirectory(uiShotUrl);

    // Inside this checkout: a sibling worktree's directory is a different path.
    expect(isInside(worktreeCache, repoRoot)).toBe(true);
    expect(isInside(uiShotCache, repoRoot)).toBe(true);
    // Outside node_modules: a junction may share that directory between
    // worktrees, and Vite's default cache lives there.
    expect(isInside(worktreeCache, resolve(repoRoot, 'node_modules'))).toBe(false);
    expect(isInside(uiShotCache, resolve(repoRoot, 'node_modules'))).toBe(false);
    // Not the same directory: two servers on one cache remove each other's
    // dependency graph.
    expect(comparable(worktreeCache)).not.toBe(comparable(uiShotCache));

    // Each server still serves its dependency graph while the other one runs.
    for (const server of [url, uiShotUrl]) {
      const { bundles } = await dependencyGraph(server);
      expect(bundles.length, 'the optimizer must expose bundles behind the dependency shims').toBeGreaterThan(0);
      for (const bundle of bundles) {
        const response = await fetch(bundle);
        expect(response.status, `${bundle.pathname} must survive the other server`).toBe(200);
      }
    }

    await stopDevServer(worktree);
    children.delete(worktree);
  }, 180000);

  it('isolates optimizer caches for concurrent servers and removes them on stop', async () => {
    const cacheRoot = await mkdtemp(join(tmpdir(), 'buildmesh-vite-caches-'));
    const owned: any[] = [];
    const dependencies: URL[] = [];
    const servedDirectories: string[] = [];
    try {
      // Warm the first graph before starting the second. Each must remain
      // readable after the other server initializes its optimizer.
      for (let i = 0; i < 2; i++) {
        const url = `http://127.0.0.1:${await freePort()}`;
        const child = await startDevServer(url, { cacheRoot });
        expect(child).not.toBeNull();
        owned.push(child);
        children.add(child);
        const entry = await fetch(`${url}/src/main.tsx`);
        expect(entry.ok).toBe(true);
        const imports = [...(await entry.text()).matchAll(/from\s+["']([^"']+\/deps\/[^"']+)["']/g)];
        expect(imports.length).toBeGreaterThan(0);
        dependencies.push(...imports.map(match => new URL(match[1], url)));
        servedDirectories.push(await dependencyDirectory(url));
      }
      // Each server serves its graph from its own directory, not from a shared
      // one: a shared cache is deleted by whichever server starts last.
      expect(comparable(servedDirectories[0])).not.toBe(comparable(servedDirectories[1]));
      const directories = await readdir(cacheRoot);
      expect(directories).toHaveLength(2);
      const metadata = await Promise.all(directories.map(async directory => {
        const file = join(cacheRoot, directory, 'deps/_metadata.json');
        await expect.poll(async () => readFile(file, 'utf8').catch(() => ''), { timeout: 30000 }).not.toBe('');
        return JSON.parse(await readFile(file, 'utf8'));
      }));
      for (const graph of metadata) expect(Object.keys(graph.optimized)).toContain('react');
      for (const dependency of dependencies) {
        const response = await fetch(dependency);
        expect(response.status, `${dependency} must survive the other optimizer's startup`).toBe(200);
      }
      await Promise.all(owned.map(child => stopDevServer(child)));
      expect(await readdir(cacheRoot)).toEqual([]);
      for (const child of owned) children.delete(child);
    } finally {
      for (const child of owned) await stopDevServer(child);
      await rm(cacheRoot, { recursive: true, force: true });
    }
  }, 120000);

  it('starts the worktree Vite entrypoint and stops the direct child', async () => {
    const port = await freePort();
    const url = `http://127.0.0.1:${port}`;
    const child = await startDevServer(url, { timeoutMs: 15000 });
    expect(child).not.toBeNull();
    children.add(child);

    const response = await fetch(url);
    expect(response.ok).toBe(true);
    expect(child.exitCode).toBeNull();

    await stopDevServer(child);
    expect(child.killed).toBe(true);
    children.delete(child);
    await expect(fetch(url)).rejects.toThrow();
  }, 60000);

  it('gives up on a listener that accepts connections but never answers, within its budget', async () => {
    // Each readiness probe is bounded by the time left in the budget, so a probe
    // that starts near the deadline cannot run a whole window past it — the
    // overrun issue #2063 exists to prevent.
    //
    // Driven through the reuse path because it spawns no Vite child, so the phase
    // is the polling loop alone and the total does not depend on when a child
    // happens to exit.
    const timeoutMs = 7000;
    const hung = await listening();

    const startedAt = Date.now();
    try {
      await expect(startDevServer(`http://127.0.0.1:${hung.port}`, { timeoutMs })).rejects.toThrow(
        'is listening but did not answer',
      );
      // Within the budget, with a margin far smaller than the overrun it
      // rejects: the broken run took 10.3s, so a 2s margin still catches it
      // while leaving room for a loaded machine.
      expect(Date.now() - startedAt).toBeLessThan(timeoutMs + 2000);
    } finally {
      await hung.close();
    }
  }, 60000);

  it('reuses a live dev server whose first response is slower than a probe', async () => {
    // A server that already owns the URL must be reused even if its first
    // response is slow. Deciding reuse by HTTP alone made a slow server look
    // dead and spawned a second Vite on a taken port, breaking the documented
    // contract that this returns null when the URL is already served (#2063).
    const slowResponseMs = 6000;
    const slow = await listening((socket) => {
      socket.on('data', () => {
        setTimeout(
          () => socket.end('HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok'),
          slowResponseMs,
        );
      });
    });

    try {
      // null is the contract: the URL is served, so nothing is spawned and the
      // response is waited out rather than abandoned.
      expect(await startDevServer(`http://127.0.0.1:${slow.port}`, { timeoutMs: 30000 })).toBeNull();
    } finally {
      await slow.close();
    }
  }, 60000);
});
