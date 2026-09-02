import { spawn } from 'child_process';
import { dirname, resolve } from 'path';
import { fileURLToPath } from 'url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const viteEntrypoint = resolve(repoRoot, 'node_modules', 'vite', 'bin', 'vite.js');
const defaultTimeoutMs = 60000;

function isReady(url) {
  return fetch(url).then((response) => response.ok).catch(() => false);
}

function rememberOutput(output, chunk) {
  const next = `${output}${chunk}`;
  return next.slice(-4000);
}

function outputDetails(output) {
  const trimmed = output.trim();
  return trimmed ? `\n${trimmed}` : '';
}

function describeExit(child, output) {
  const code = child.exitCode === null ? 'unknown' : child.exitCode;
  return `Vite dev server exited with code ${code}.${outputDetails(output)}`;
}

/**
 * Stop a Vite process that this module started and wait briefly for it to
 * exit. Vite is launched as a direct Node child, so no shell process tree or
 * Windows-specific taskkill fallback is needed.
 */
export function stopDevServer(child) {
  if (!child?.pid || child.exitCode !== null) return Promise.resolve();

  child.kill();
  return new Promise((resolvePromise) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolvePromise();
    };
    const timeout = setTimeout(finish, 2000);
    child.once('close', finish);
  });
}

/**
 * Start this worktree's Vite server and resolve once its URL answers.
 * Returns null when another process already owns the requested URL.
 */
export async function startDevServer(mockUrl, { timeoutMs = defaultTimeoutMs } = {}) {
  if (await isReady(mockUrl)) {
    console.log(`Reusing dev server already listening at ${mockUrl}`);
    return null;
  }

  console.log('Starting Vite dev server …');
  const server = new URL(mockUrl);
  const viteArgs = [];
  if (server.port && server.port !== '1420') {
    viteArgs.push('--host', server.hostname, '--port', server.port);
  }

  const child = spawn(process.execPath, [viteEntrypoint, ...viteArgs], {
    cwd: repoRoot,
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: false,
    shell: false,
  });
  let processError;
  let output = '';
  child.stdout?.on('data', (chunk) => { output = rememberOutput(output, chunk); });
  child.stderr?.on('data', (chunk) => { output = rememberOutput(output, chunk); });
  child.once('error', (error) => { processError = error; });

  try {
    const startedAt = Date.now();
    while (Date.now() - startedAt < timeoutMs) {
      if (processError) {
        throw new Error(`Could not start the Vite dev server: ${processError.message}`);
      }
      if (child.exitCode !== null) {
        throw new Error(describeExit(child, output));
      }
      if (await isReady(mockUrl)) {
        console.log(`Dev server ready at ${mockUrl}`);
        return child;
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
    }
  } catch (error) {
    await stopDevServer(child);
    throw error;
  }

  await stopDevServer(child);
  throw new Error(`Dev server did not come up at ${mockUrl} within ${timeoutMs}ms.${outputDetails(output)}`);
}
