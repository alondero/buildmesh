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
  }, 30000);
});
