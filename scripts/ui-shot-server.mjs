import { spawn } from 'child_process';
import { connect } from 'net';
import { createRequire } from 'module';
import { dirname, resolve } from 'path';
import { fileURLToPath } from 'url';
import { DEV_SERVER_STARTUP_MS, DEV_READY_POLL_MS, DEV_LISTEN_PROBE_MS, DEV_SERVER_STOP_MS } from './ui-shot-budgets.mjs';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const vitePackageJson = require.resolve('vite/package.json');
const viteEntrypoint = resolve(dirname(vitePackageJson), 'bin', 'vite.js');
// The startup budget lives in the budgets module, which
// `tests/integration/ui-shot.test.ts` also reads, so the wrapper deadline prices
// the real value rather than a hand-copied literal that would drift when this
// changes (issue #2049 class: a wrapper tighter than its child reports a
// transport error that reads like "start the dev server" when `--serve` already
// started one).
const defaultTimeoutMs = DEV_SERVER_STARTUP_MS;

function isReady(url, timeoutMs) {
  // Each probe is bounded by the time left in the startup budget, so a probe
  // that starts near the deadline still cannot run past it (#2063). There is no
  // separate per-probe cap: one would stop a live dev server whose first response
  // is merely slow from ever answering, which broke the reuse contract.
  return fetch(url, { signal: AbortSignal.timeout(timeoutMs) }).then((response) => response.ok).catch(() => false);
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
  // A child killed by a signal has no exit code, so name the signal: it is the
  // one fact that explains the exit.
  const how = child.exitCode === null
    ? (child.signalCode ? `signal ${child.signalCode}` : 'unknown')
    : `code ${child.exitCode}`;
  return `Vite dev server exited with ${how}.${outputDetails(output)}`;
}

// A child killed by a signal has `exitCode === null` and a `signalCode` instead,
// so both count as exited; otherwise a dead dev server goes unnoticed.
const hasExited = child => child.exitCode !== null || child.signalCode !== null;

/**
 * Stop a Vite process that this module started and wait briefly for it to
 * exit. Vite is launched as a direct Node child, so no shell process tree or
 * Windows-specific taskkill fallback is needed.
 */
export function stopDevServer(child) {
  if (!child?.pid || hasExited(child)) return Promise.resolve();

  child.kill();
  return new Promise((resolvePromise) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolvePromise();
    };
    const timeout = setTimeout(finish, DEV_SERVER_STOP_MS);
    child.once('close', finish);
  });
}

/**
 * Whether something already accepts connections on this host and port.
 *
 * A TCP handshake answers "is this URL already served?" without waiting for an
 * HTTP response. Deciding reuse this way matters because each HTTP probe is
 * bounded by the startup budget: a live server whose first response is slow
 * would look exactly like a dead one, and the CLI would spawn a second Vite on
 * the same port, contradicting this function's contract (issue #2063).
 */
function isListening(host, port) {
  return new Promise((resolvePromise) => {
    const socket = connect({ host, port });
    const settle = listening => {
      socket.destroy();
      resolvePromise(listening);
    };
    socket.setTimeout(DEV_LISTEN_PROBE_MS, () => settle(false));
    socket.once('connect', () => settle(true));
    socket.once('error', () => settle(false));
  });
}

/**
 * Start this worktree's Vite server and resolve once its URL answers.
 * Returns null when another process already owns the requested URL.
 */
export async function startDevServer(mockUrl, { timeoutMs = defaultTimeoutMs } = {}) {
  // The clock starts before the reuse check, so the phase really is bounded by
  // `timeoutMs`. A probe may only spend what is left of it, otherwise a probe
  // starting just before the deadline runs to its own timeout past it (#2063).
  const startedAt = Date.now();
  const remaining = () => Math.max(1, timeoutMs - (Date.now() - startedAt));
  const target = new URL(mockUrl);
  const host = target.hostname;
  const port = Number(target.port || (target.protocol === 'https:' ? 443 : 80));

  if (await isListening(host, port)) {
    console.log(`Reusing dev server already listening at ${mockUrl}`);
    // Something holds the port but may still be warming up, so wait for it to
    // answer rather than returning immediately — and rather than spawning a
    // second server on a port that is already taken.
    //
    // The port is known to be served, so these probes spend the whole remaining
    // budget rather than a slice of it: a slow first response must still be
    // waited out, or a live dev server looks dead (#2063). The spawn path below
    // makes the same choice, because the remaining-time clip is what keeps the
    // phase inside its deadline either way.
    while (Date.now() - startedAt < timeoutMs) {
      if (await isReady(mockUrl, remaining())) return null;
      await new Promise((resolvePromise) => setTimeout(resolvePromise, Math.min(DEV_READY_POLL_MS, remaining())));
    }
    throw new Error(`Dev server at ${mockUrl} is listening but did not answer within ${timeoutMs}ms.`);
  }

  console.log('Starting Vite dev server …');
  const viteArgs = [];
  if (target.port && target.port !== '1420') {
    viteArgs.push('--host', target.hostname, '--port', target.port);
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
    while (Date.now() - startedAt < timeoutMs) {
      if (processError) {
        throw new Error(`Could not start the Vite dev server: ${processError.message}`);
      }
      if (hasExited(child)) {
        throw new Error(describeExit(child, output));
      }
      if (await isReady(mockUrl, remaining())) {
        console.log(`Dev server ready at ${mockUrl}`);
        return child;
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, Math.min(DEV_READY_POLL_MS, remaining())));
    }
  } catch (error) {
    await stopDevServer(child);
    throw error;
  }

  await stopDevServer(child);
  throw new Error(`Dev server did not come up at ${mockUrl} within ${timeoutMs}ms.${outputDetails(output)}`);
}
