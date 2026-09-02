import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createServer as createTcpServer } from 'node:net';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const uiShot = resolve(repoRoot, 'scripts', 'ui-shot.mjs');
const circuitSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-circuit.steps.mjs');

async function freePort() {
  const server = createTcpServer();
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

function runUiShot(args, timeoutMs = 60000) {
  return new Promise<{ code: number | null; stdout: string; stderr: string }>((resolvePromise, reject) => {
    const child = spawn(process.execPath, [uiShot, ...args], {
      cwd: repoRoot,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error(`ui-shot did not finish within ${timeoutMs}ms\n${stdout}\n${stderr}`));
    }, timeoutMs);
    child.once('error', (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once('close', (code) => {
      clearTimeout(timeout);
      resolvePromise({ code, stdout, stderr });
    });
  });
}

async function serveHtml(html) {
  const port = await freePort();
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'text/html' });
    response.end(html);
  });
  await new Promise<void>((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolvePromise);
  });
  return { server, url: `http://127.0.0.1:${port}` };
}

describe('ui-shot mock mode', () => {
  it('serves the fixture UI, drives a circuit, and writes a screenshot', async () => {
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-'));
    try {
      const port = await freePort();
      const output = join(folder, 'circuit.png');
      const result = await runUiShot([
        '--out', output,
        '--mock',
        '--serve',
        '--mock-url', `http://127.0.0.1:${port}`,
        '--steps', circuitSteps,
      ]);

      expect(result.code).toBe(0);
      expect(result.stdout).toContain('Saved');
      expect((await stat(output)).size).toBeGreaterThan(0);
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  }, 90000);

  it('reports root mount failure and browser console errors', async () => {
    const { server, url } = await serveHtml(
      '<div id="root"></div><script>console.error("mock mount exploded")</script>'
    );
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-'));
    try {
      const output = join(folder, 'should-not-exist.png');
      const result = await runUiShot(['--out', output, '--mock', '--mock-url', url], 30000);

      expect(result.code).toBe(1);
      expect(result.stderr).toContain('#root never populated within 15s');
      expect(result.stderr).toContain('Page errors: mock mount exploded');
      await expect(readFile(output)).rejects.toThrow();
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, 45000);
});
