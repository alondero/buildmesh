#!/usr/bin/env node
// A worktree-owned Vite process. The private control endpoint proves ownership
// before stopping it, so a stale PID cannot kill a different process.
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { closeSync, mkdirSync, openSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(import.meta.url);
const root = resolve(dirname(script), '..');
const stateDir = join(root, '.harness');
const statePath = join(stateDir, 'dev-server.json');
const logPath = join(stateDir, 'logs', 'dev-server.log');
const lockPath = join(stateDir, 'dev-server.lock');
const delay = ms => new Promise(resolvePromise => setTimeout(resolvePromise, ms));

function alive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) throw new Error('Invalid recorded PID');
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}

function readState(path = statePath) {
  try { return JSON.parse(readFileSync(path, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

async function control(state, action) {
  const response = await fetch(`http://127.0.0.1:${state.controlPort}/${action}`, {
    method: 'POST', headers: { Authorization: `Bearer ${state.token}` },
    signal: AbortSignal.timeout(10000),
  });
  const reply = await response.json();
  if (!response.ok || reply.pid !== state.pid || reply.token !== state.token) {
    throw new Error('Dev server ownership could not be confirmed; no process was stopped');
  }
}

function describe(state) {
  console.log(`OK - Vite ready at ${state.url}\nPID: ${state.pid}\nLog: ${logPath}\nState: ${statePath}`);
}

async function start({ port, timeoutMs }) {
  const deadline = Date.now() + timeoutMs;
  const remaining = () => Math.max(1, deadline - Date.now());
  const previous = readState();
  if (previous && alive(previous.pid)) {
    await control(previous, 'status');
    if (previous.port !== port) throw new Error(`This worktree already serves ${previous.url}; run npm run dev:stop first`);
    describe(previous);
    return;
  }
  if (previous) unlinkSync(statePath);
  const log = openSync(logPath, 'a');
  const environment = { ...process.env, NODE_ENV: 'development', NO_COLOR: '1' };
  delete environment.FORCE_COLOR;
  let child;
  try {
    child = spawn(process.execPath, [script, 'worker', String(port), randomUUID()], {
      cwd: root, detached: true, windowsHide: true, shell: false,
      stdio: ['ignore', log, log, 'ipc'],
      env: environment,
    });
  } finally { closeSync(log); }
  if (child.pid) console.log(`Starting Vite PID: ${child.pid}`);
  try {
    const state = await new Promise((resolvePromise, reject) => {
      const timer = setTimeout(() => finish(new Error(`Vite readiness timed out after ${timeoutMs}ms; see ${logPath}`)), remaining());
      const finish = (error, value) => {
        clearTimeout(timer);
        child.removeListener('message', onMessage);
        child.removeListener('exit', onExit);
        child.removeListener('error', onError);
        if (error) reject(error); else resolvePromise(value);
      };
      const onMessage = value => finish(null, value);
      const onExit = code => finish(new Error(`Vite exited with ${code}; see ${logPath}`));
      const onError = error => finish(error);
      child.once('message', onMessage);
      child.once('exit', onExit);
      child.once('error', onError);
    });
    // HTTP readiness belongs to this process: Vite uses strictPort and sends
    // the receipt only after listen succeeds, before we probe the URL.
    const response = await fetch(state.url, { signal: AbortSignal.timeout(remaining()) });
    if (!response.ok) throw new Error(`Vite readiness returned HTTP ${response.status}`);
    writeFileSync(statePath, JSON.stringify(state, null, 2) + '\n', { mode: 0o600 });
    child.send('detach');
    child.disconnect();
    child.unref();
    describe(state);
  } catch (error) {
    child.kill();
    if (child.exitCode === null && child.signalCode === null) {
      await new Promise(resolvePromise => {
        const timer = setTimeout(resolvePromise, 5000);
        child.once('exit', () => { clearTimeout(timer); resolvePromise(); });
      });
    }
    if (child.connected) child.disconnect();
    child.unref();
    if (readState()?.pid === child.pid) unlinkSync(statePath);
    throw error;
  }
}

async function stop() {
  const state = readState();
  if (!state) { console.log('No detached Vite server recorded for this worktree'); return; }
  if (alive(state.pid)) {
    await control(state, 'stop');
    const deadline = Date.now() + 5000;
    while (alive(state.pid) && Date.now() < deadline) await delay(50);
    if (alive(state.pid)) throw new Error(`Vite PID ${state.pid} has not exited; ownership record retained`);
  }
  unlinkSync(statePath);
  console.log(`Stopped worktree Vite PID ${state.pid}`);
}

async function worker(port, token) {
  let detached = false;
  process.on('message', message => { if (message === 'detach') detached = true; });
  // A killed startup must not leave an unrecorded server behind.
  process.on('disconnect', () => { if (!detached) process.exit(1); });
  const { createServer: createViteServer } = await import('vite');
  const vite = await createViteServer({
    root, server: { host: '127.0.0.1', port, strictPort: true }, clearScreen: false,
  });
  await vite.listen();
  const server = createServer(async (request, response) => {
    if (request.method !== 'POST' || request.headers.authorization !== `Bearer ${token}` ||
        !['/status', '/stop'].includes(request.url)) {
      response.writeHead(403).end();
      return;
    }
    try {
      if (request.url === '/stop') {
        await vite.close();
        // Vite's config/dependency bundler owns an esbuild service process.
        // Close it before exiting so its Windows cwd handle is released too.
        const esbuild = await import('esbuild');
        esbuild.stop();
      }
      response.setHeader('Connection', 'close');
      response.setHeader('Content-Type', 'application/json');
      response.end(JSON.stringify({ pid: process.pid, token }));
      if (request.url === '/stop') server.close(() => process.exit(0));
    } catch (error) {
      console.error(error);
      response.writeHead(500).end('{}');
    }
  });
  await new Promise((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolvePromise);
  });
  process.send({ pid: process.pid, port, token, controlPort: server.address().port, url: `http://127.0.0.1:${port}` });
}

async function main() {
  const [action, ...args] = process.argv.slice(2);
  if (action === 'worker') { await worker(Number(args[0]), args[1]); return; }
  if (!['start', 'stop'].includes(action)) throw new Error('Use start [--port 3999] [--timeout-seconds 60] or stop');
  const options = { port: 3999, timeoutMs: 60000 };
  for (let i = 0; i < args.length; i += 2) {
    const value = Number(args[i + 1]);
    if (!Number.isInteger(value) || value <= 0) throw new Error(`Invalid value for ${args[i]}`);
    if (args[i] === '--port' && value <= 65535) options.port = value;
    else if (args[i] === '--timeout-seconds') options.timeoutMs = value * 1000;
    else throw new Error(`Unknown option ${args[i]}`);
  }
  mkdirSync(dirname(logPath), { recursive: true });
  // Serialize CLI operations, reclaiming a lock only when its owner is dead.
  try { writeFileSync(lockPath, JSON.stringify({ pid: process.pid }), { flag: 'wx' }); }
  catch (error) {
    if (error.code !== 'EEXIST') throw error;
    const lock = readState(lockPath);
    if (!lock || alive(lock.pid)) throw new Error('Another dev server operation holds the worktree lock');
    unlinkSync(lockPath);
    writeFileSync(lockPath, JSON.stringify({ pid: process.pid }), { flag: 'wx' });
  }
  try { if (action === 'start') await start(options); else await stop(); }
  finally { unlinkSync(lockPath); }
}

main().catch(error => { console.error(`ERROR: ${error.message}`); process.exit(1); });
