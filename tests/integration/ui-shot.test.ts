import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createServer as createTcpServer } from 'node:net';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
  DEV_SERVER_STARTUP_MS,
  NAVIGATION_TIMEOUT_MS,
  MOUNT_TIMEOUT_MS,
  UI_SHOT_STEP_BUDGETS_MS,
} from '../../scripts/ui-shot-budgets.mjs';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const uiShot = resolve(repoRoot, 'scripts', 'ui-shot.mjs');
const circuitSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-circuit.steps.mjs');
const circuitFixtures = resolve(repoRoot, 'tests', 'integration', 'ui-shot-circuit.fixtures.mjs');
const nodeHeaderSteps = resolve(repoRoot, 'tests', 'integration', 'ui-shot-node-header.steps.mjs');
const nodeHeaderFixtures = resolve(repoRoot, 'tests', 'integration', 'ui-shot-node-header.fixtures.mjs');

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

// Budget ordering matters, and the arithmetic has to actually hold against the
// values the child really uses. The child spends, sequentially:
// dev-server startup, then `page.goto`, then the `#root` mount wait, then a
// selector-visible wait. The wrapper must exceed that whole sum or it kills the
// child mid-flight and reports a bare transport error instead of the real
// diagnostic (issue #2049 class). The per-test timeout then exceeds the wrapper,
// so the wrapper's diagnostic — which carries the child's stdout/stderr — is what
// fails the run.
//
// The child's budgets are IMPORTED, not copied: a literal here would drift the
// moment a timeout is raised in the scripts and the check below would keep
// passing while the wrapper silently fell below the real worst case again.
const CHILD_WORST_CASE_MS = UI_SHOT_STEP_BUDGETS_MS;
const WRAPPER_DEADLINE_MS = CHILD_WORST_CASE_MS + 30000;
const TEST_DEADLINE_MS = WRAPPER_DEADLINE_MS + 30000;

function runUiShot(args, timeoutMs = WRAPPER_DEADLINE_MS) {
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

// This test serves its own static HTML (no `--serve`), so the child spends only
// navigation plus the mount wait before reporting the failure. It still needs a
// wrapper above that sum, and still needs enough budget for Chromium to start
// while competing with the rest of the suite (issue #2049).
const MOUNT_FAILURE_DEADLINE_MS = NAVIGATION_TIMEOUT_MS + MOUNT_TIMEOUT_MS + 30000;

describe('ui-shot mock mode', () => {
  it('groups, reloads, swaps and ungroups nodes through pointer and keyboard interactions', async () => {
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-groups-'));
    try {
      const port = await freePort();
      const result = await runUiShot([
        '--out', join(folder, 'groups.png'), '--mock', '--serve',
        '--mock-url', `http://127.0.0.1:${port}`,
        '--fixtures', resolve(repoRoot, 'tests/integration/ui-shot-node-groups.fixtures.mjs'),
        '--steps', resolve(repoRoot, 'tests/integration/ui-shot-node-groups.steps.mjs'),
      ]);
      expect(result.code, result.stderr).toBe(0);
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  }, TEST_DEADLINE_MS);

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
        '--fixtures', circuitFixtures,
        '--steps', circuitSteps,
      ]);

      expect(result.code, result.stderr).toBe(0);
      expect(result.stdout).toContain('Saved');
      expect((await stat(output)).size).toBeGreaterThan(0);
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  }, TEST_DEADLINE_MS);

  it('keeps the node title and trailing close visible in a 240px pane', async () => {
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-header-'));
    try {
      const port = await freePort();
      const output = join(folder, 'header.png');
      const result = await runUiShot([
        '--out', output,
        '--mock',
        '--serve',
        '--mock-url', `http://127.0.0.1:${port}`,
        '--fixtures', nodeHeaderFixtures,
        '--steps', nodeHeaderSteps,
        '--selector', '[data-testid=grid-node-header]',
      ]);

      expect(result.code, result.stderr).toBe(0);
      expect(result.stdout).toContain('Saved');
      expect((await stat(output)).size).toBeGreaterThan(0);
    } finally {
      await rm(folder, { recursive: true, force: true });
    }
  }, TEST_DEADLINE_MS);

  it('reports root mount failure and browser console errors', async () => {
    const { server, url } = await serveHtml(
      '<div id="root"></div><script>console.error("mock mount exploded")</script>'
    );
    const folder = await mkdtemp(join(tmpdir(), 'buildmesh-ui-shot-'));
    try {
      const output = join(folder, 'should-not-exist.png');
      const result = await runUiShot(['--out', output, '--mock', '--mock-url', url], MOUNT_FAILURE_DEADLINE_MS);

      expect(result.code).toBe(1);
      expect(result.stderr).toContain('#root never populated within 15s');
      expect(result.stderr).toContain('Page errors: mock mount exploded');
      await expect(readFile(output)).rejects.toThrow();
    } finally {
      await rm(folder, { recursive: true, force: true });
      await new Promise<void>((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()));
    }
  }, TEST_DEADLINE_MS);

  it('keeps its wrapper deadline above the child it supervises', () => {
    // The failure this guards is silent: a wrapper tighter than the child just
    // produces a transport error that reads like "start the dev server" when
    // `--serve` already started one. Assert the arithmetic against the values
    // the scripts actually use (imported from `ui-shot-budgets.mjs`), so
    // raising a child timeout widens the wrapper automatically.
    expect(CHILD_WORST_CASE_MS).toBeGreaterThanOrEqual(
      DEV_SERVER_STARTUP_MS + NAVIGATION_TIMEOUT_MS,
    );
    expect(WRAPPER_DEADLINE_MS).toBeGreaterThan(CHILD_WORST_CASE_MS);
    expect(TEST_DEADLINE_MS).toBeGreaterThan(WRAPPER_DEADLINE_MS);
  });
});
