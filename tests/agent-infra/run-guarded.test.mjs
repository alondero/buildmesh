import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parseArgs } from '../../scripts/ci/run-guarded.mjs';

const script = fileURLToPath(new URL('../../scripts/ci/run-guarded.mjs', import.meta.url));

function runGuard(args, { timeoutMs = 30000, env } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [script, ...args], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: env ? { ...process.env, ...env } : process.env,
    });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const killer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`run-guarded did not exit within ${timeoutMs}ms`));
    }, timeoutMs);
    child.on('error', (err) => { clearTimeout(killer); reject(err); });
    child.on('close', (code) => { clearTimeout(killer); resolve({ code, stdout, stderr }); });
  });
}

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), 'run-guarded-'));
  return Promise.resolve()
    .then(() => fn(dir))
    .finally(() => rmSync(dir, { recursive: true, force: true }));
}

test('parseArgs keeps every token after -- verbatim, including later -- tokens', () => {
  const parsed = parseArgs([
    '--minutes', '30',
    '--kill-grace-seconds', '5',
    '--log', 'cargo-db.log',
    '--label', 'The db shard',
    '--', 'cargo', 'test', '--locked', '--', '--test-threads=1', 'db::',
  ]);
  assert.equal(parsed.minutes, 30);
  assert.equal(parsed.killGraceSeconds, 5);
  assert.equal(parsed.log, 'cargo-db.log');
  assert.equal(parsed.label, 'The db shard');
  assert.deepEqual(parsed.command, ['cargo', 'test', '--locked', '--', '--test-threads=1', 'db::']);
});

test('parseArgs rejects a missing command or deadline', () => {
  assert.throws(() => parseArgs(['--minutes', '1']), /Usage/);
  assert.throws(() => parseArgs(['--', 'cargo', 'test']), /Usage/);
  assert.throws(() => parseArgs(['--minutes', '0', '--', 'cargo']), /Usage/);
  assert.throws(() => parseArgs(['--bogus', '1', '--', 'cargo']), /Usage/);
});

test('exits with the guarded command status and streams its output to the log', async () => {
  await withTempDir(async (dir) => {
    const log = join(dir, 'out.log');
    const res = await runGuard([
      '--minutes', '5', '--log', log, '--label', 'demo',
      '--', process.execPath, '-e', "console.log('guarded-hello')",
    ]);
    assert.equal(res.code, 0, res.stderr);
    assert.match(res.stdout, /guarded-hello/);
    assert.match(readFileSync(log, 'utf8'), /guarded-hello/);
  });
});

test('propagates a failing guarded command exit code', async () => {
  const res = await runGuard(['--minutes', '5', '--', process.execPath, '-e', 'process.exit(3)']);
  assert.equal(res.code, 3);
});

test('kills a hung command at the deadline, exits 124, and annotates the step', async () => {
  await withTempDir(async (dir) => {
    const started = Date.now();
    const res = await runGuard([
      '--minutes', '0.02', '--kill-grace-seconds', '1',
      '--label', 'The services shard',
      '--', process.execPath, '-e', 'setInterval(() => {}, 1000)',
    ]);
    const elapsed = Date.now() - started;
    assert.equal(res.code, 124, res.stderr);
    assert.match(res.stdout, /::error::The services shard exceeded .* minutes and was killed/);
    assert.ok(elapsed < 25000, `guard took ${elapsed}ms to fire`);
  });
});

test('keeps output produced before the deadline in the log file', async () => {
  await withTempDir(async (dir) => {
    const log = join(dir, 'out.log');
    const res = await runGuard([
      '--minutes', '0.02', '--kill-grace-seconds', '1', '--log', log, '--label', 'demo',
      '--', process.execPath, '-e', "console.log('before-the-hang'); setInterval(() => {}, 1000)",
    ]);
    assert.equal(res.code, 124);
    assert.match(readFileSync(log, 'utf8'), /before-the-hang/);
  });
});

test('kills the whole process tree, not just the direct child', async () => {
  await withTempDir(async (dir) => {
    const marker = join(dir, 'marker.txt');
    const grandchild = "setInterval(() => require('node:fs').appendFileSync(process.env.GUARDED_MARKER, 'x\\n'), 50)";
    const childCode = [
      "const { spawn } = require('node:child_process');",
      `spawn(process.execPath, ['-e', ${JSON.stringify(grandchild)}], { stdio: 'ignore', env: process.env });`,
      'setInterval(() => {}, 1000);',
    ].join('');
    const res = await runGuard([
      '--minutes', '0.03', '--kill-grace-seconds', '1', '--label', 'tree',
      '--', process.execPath, '-e', childCode,
    ], { timeoutMs: 30000, env: { GUARDED_MARKER: marker } });
    assert.equal(res.code, 124, res.stderr);
    const sizeAtExit = existsSync(marker) ? statSync(marker).size : 0;
    assert.ok(sizeAtExit >= 4, `grandchild never started appending (marker ${sizeAtExit} bytes)`);
    await new Promise((resolve) => setTimeout(resolve, 800));
    assert.equal(statSync(marker).size, sizeAtExit, 'grandchild still appends after the guard exited');
  });
});

test('escalates to SIGKILL when the command ignores the terminate signal', {
  skip: process.platform === 'win32' ? 'signal semantics differ on Windows' : false,
}, async () => {
  const res = await runGuard([
    '--minutes', '0.02', '--kill-grace-seconds', '1', '--label', 'stubborn',
    '--', process.execPath, '-e', "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)",
  ]);
  assert.equal(res.code, 124);
});

test('rejects a missing command with a usage error', async () => {
  const res = await runGuard(['--minutes', '1']);
  assert.equal(res.code, 2);
});
