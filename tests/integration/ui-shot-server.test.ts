import { createServer } from 'node:net';
import { afterEach, describe, expect, it } from 'vitest';
import { startDevServer, stopDevServer } from '../../scripts/ui-shot-server.mjs';

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

describe('ui-shot Vite server', () => {
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
