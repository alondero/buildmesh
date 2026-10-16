// Behaviour of the shard runner's process wrapper: a test process is finished
// when it exits, not when every descendant that inherited its pipes lets go.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { run } from '../../scripts/rust-test-shards.mjs';

// Short enough to keep the suite quick, long enough that the child's own output
// is in the pipe before the cap expires.
const DRAIN_CAP_MS = 500;

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), 'buildmesh-shard-run-'));
  return Promise.resolve()
    .then(() => fn(dir))
    .finally(() => rmSync(dir, { recursive: true, force: true }));
}

// The grandchild outlives the child, so nothing but the drain can end the run.
function withWatchdog(work, ms, message) {
  let timer;
  const guard = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), ms);
  });
  return Promise.race([work, guard]).finally(() => clearTimeout(timer));
}

function killTree(pid) {
  if (!Number.isFinite(pid) || pid <= 0) return;
  if (process.platform === 'win32') spawnSync('taskkill', ['/pid', String(pid), '/T', '/F'], { stdio: 'ignore' });
  else { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
}

function grandchildPid(pidFile) {
  if (!existsSync(pidFile)) return 0;
  return Number(readFileSync(pidFile, 'utf8').trim()) || 0;
}

// Issue #2374: a Rust test process that exits while a descendant (a lingering
// conhost, a stray cargo child) still holds the stdout/stderr pipes it inherited
// is finished. Waiting for `close` left the local wrapper pending indefinitely.
test('a descendant holding the inherited pipes cannot keep a finished process pending', async () => {
  await withTempDir(async (dir) => {
    const pidFile = join(dir, 'grandchild.pid');
    const grandchild = 'setInterval(() => {}, 1000)';
    // `unref` matters: a Node parent waits for its own spawned child, so the
    // direct child would never exit — and then nothing would be holding the
    // pipes after it. This is the shape from the issue: the child exits, the
    // grandchild stays.
    const child = [
      "const { spawn } = require('node:child_process');",
      "const { writeFileSync } = require('node:fs');",
      `const held = spawn(process.execPath, ['-e', ${JSON.stringify(grandchild)}], { stdio: 'inherit', detached: true });`,
      'held.unref();',
      'writeFileSync(process.env.SHARD_GRANDCHILD_PID, String(held.pid));',
      "console.log('test result: ok. 1 passed; 0 failed');",
    ].join('');

    const started = Date.now();
    let result;
    try {
      result = await withWatchdog(
        run('held-pipes', process.execPath, ['-e', child], {
          drainCapMs: DRAIN_CAP_MS,
          env: { ...process.env, SHARD_GRANDCHILD_PID: pidFile },
        }),
        20_000,
        `run() stayed pending ${20_000}ms after the child exited while its descendant held the pipes`,
      );
    } finally {
      // The descendant is deliberately never killed by the runner, so the test
      // owns it: leaving it running would hold a pipe in the test runner too.
      killTree(grandchildPid(pidFile));
    }

    assert.ok(grandchildPid(pidFile) > 0, 'the fixture never started its descendant');
    assert.equal(result.code, 0, result.output);
    assert.match(result.stdout, /test result: ok\. 1 passed; 0 failed/);
    // Only the cap could have ended this run: the descendant still owned the
    // write end when the result was reported, so the fixture did hold the
    // pipes rather than passing for a reason of its own.
    assert.ok(Date.now() - started >= DRAIN_CAP_MS - 50, 'the run ended before the drain cap, so the descendant was not holding the pipes');
  });
});

test('a normal shard run keeps its output and a failing exit code', async () => {
  const result = await run('failing-shard', process.execPath, [
    '-e',
    "console.log('test result: FAILED. 0 passed; 1 failed'); process.exit(101)",
  ], { drainCapMs: DRAIN_CAP_MS });
  assert.equal(result.code, 101, result.output);
  assert.match(result.output, /test result: FAILED\. 0 passed; 1 failed/);
});

test('a process that cannot be started reports the spawn failure instead of hanging', async () => {
  const result = await run('missing-shard', join(tmpdir(), 'buildmesh-no-such-binary-2374'), [], {
    drainCapMs: DRAIN_CAP_MS,
  });
  assert.equal(result.code, 127, result.output);
  assert.match(result.output, /ENOENT/);
});