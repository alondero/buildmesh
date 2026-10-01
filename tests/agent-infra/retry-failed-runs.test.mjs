import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  isPrStateRetryable,
  main,
  normaliseBranch,
  selectRunsToRetry,
} from '../../scripts/ci/retry-failed-runs.mjs';

const NOW = Date.parse('2026-10-01T12:00:00Z');
const HOUR = 3_600_000;

function run(overrides = {}) {
  return {
    databaseId: 1,
    status: 'completed',
    conclusion: 'failure',
    event: 'pull_request',
    headBranch: 'feature-a',
    createdAt: new Date(NOW - 1 * HOUR).toISOString(),
    attempt: 1,
    url: 'https://example.test/runs/1',
    ...overrides,
  };
}

test('selects the newest failed first attempt for an open pull request', () => {
  const candidate = run();
  const selected = selectRunsToRetry([candidate], { now: NOW, prStateByBranch: new Map([['feature-a', 'OPEN']]) });
  assert.deepEqual(selected.map((r) => r.databaseId), [1]);
});

test('ignores runs that are not completed failures or cancellations', () => {
  const runs = [
    run({ databaseId: 2, conclusion: 'success' }),
    run({ databaseId: 3, conclusion: 'neutral' }),
    run({ databaseId: 4, status: 'in_progress', conclusion: null }),
    run({ databaseId: 5, conclusion: 'skipped' }),
  ];
  assert.deepEqual(selectRunsToRetry(runs, { now: NOW, prStateByBranch: new Map([['feature-a', 'open']]) }), []);
});

test('gives a run only one automatic retry', () => {
  const selected = selectRunsToRetry([run({ attempt: 2 })], {
    now: NOW,
    prStateByBranch: new Map([['feature-a', 'open']]),
  });
  assert.deepEqual(selected, []);
});

test('ignores failures older than the freshness window', () => {
  const pr = new Map([['feature-a', 'open']]);
  assert.equal(selectRunsToRetry([run({ createdAt: new Date(NOW - 7 * HOUR).toISOString() })], { now: NOW, prStateByBranch: pr }).length, 0);
  assert.equal(selectRunsToRetry([run({ createdAt: new Date(NOW - 5 * HOUR).toISOString() })], { now: NOW, prStateByBranch: pr }).length, 1);
});

test('retries only the newest run per event and head branch', () => {
  const pr = new Map([['feature-a', 'open']]);
  const superseded = selectRunsToRetry([
    run({ databaseId: 10, createdAt: new Date(NOW - 2 * HOUR).toISOString() }),
    run({ databaseId: 11, conclusion: 'success', createdAt: new Date(NOW - 1 * HOUR).toISOString() }),
  ], { now: NOW, prStateByBranch: pr });
  assert.deepEqual(superseded, []);

  const newestFailed = selectRunsToRetry([
    run({ databaseId: 10, createdAt: new Date(NOW - 2 * HOUR).toISOString() }),
    run({ databaseId: 11, createdAt: new Date(NOW - 1 * HOUR).toISOString() }),
  ], { now: NOW, prStateByBranch: pr });
  assert.deepEqual(newestFailed.map((r) => r.databaseId), [11]);
});

test('skips pull-request runs whose pull request is no longer open', () => {
  const runs = [run()];
  assert.equal(selectRunsToRetry(runs, { now: NOW, prStateByBranch: new Map([['feature-a', 'MERGED']]) }).length, 0);
  assert.equal(selectRunsToRetry(runs, { now: NOW, prStateByBranch: new Map([['feature-a', 'closed']]) }).length, 0);
  assert.equal(selectRunsToRetry(runs, { now: NOW, prStateByBranch: new Map() }).length, 0);
});

test('retries cancelled push runs that no newer run supersedes', () => {
  const cancelledPush = run({ event: 'push', headBranch: 'main', conclusion: 'cancelled' });
  const selected = selectRunsToRetry([cancelledPush], { now: NOW, prStateByBranch: new Map() });
  assert.deepEqual(selected.map((r) => r.databaseId), [1]);

  const superseded = selectRunsToRetry([
    cancelledPush,
    run({ databaseId: 2, event: 'push', headBranch: 'main', conclusion: 'success', createdAt: new Date(NOW - 0.5 * HOUR).toISOString() }),
  ], { now: NOW, prStateByBranch: new Map() });
  assert.deepEqual(superseded, []);
});

test('caps the retries per invocation at the newest candidates', () => {
  const pr = new Map([['a', 'open'], ['b', 'open'], ['c', 'open'], ['d', 'open']]);
  const runs = ['a', 'b', 'c', 'd'].map((branch, index) => run({
    databaseId: 100 + index,
    headBranch: branch,
    createdAt: new Date(NOW - (index + 1) * 60_000).toISOString(),
  }));
  const selected = selectRunsToRetry(runs, { now: NOW, prStateByBranch: pr, maxRetries: 3 });
  assert.deepEqual(selected.map((r) => r.databaseId), [100, 101, 102]);
});

test('normalises branch names and pull request states', () => {
  assert.equal(normaliseBranch('refs/heads/feature-a'), 'feature-a');
  assert.equal(isPrStateRetryable(new Map([['a', 'OPEN']]), 'a'), true);
  assert.equal(isPrStateRetryable(new Map([['a', 'open']]), 'a'), true);
  assert.equal(isPrStateRetryable(new Map([['a', 'MERGED']]), 'a'), false);
  assert.equal(isPrStateRetryable(new Map([['a', 'closed']]), 'a'), false);
  assert.equal(isPrStateRetryable(new Map(), 'a'), false);
});

test('main lists Build runs, re-runs the failed jobs of the selected runs', async () => {
  const calls = [];
  const gh = async (args) => {
    calls.push(args);
    if (args[0] === 'run' && args[1] === 'list') {
      return [run({ databaseId: 42, url: 'https://example.test/runs/42' })];
    }
    if (args[0] === 'pr' && args[1] === 'list') {
      return [{ headRefName: 'feature-a', state: 'OPEN' }];
    }
    return [];
  };
  const code = await main([], { gh, now: NOW });
  assert.equal(code, 0);
  const rerun = calls.find((args) => args[0] === 'run' && args[1] === 'rerun');
  assert.ok(rerun, 'expected a gh run rerun call');
  assert.deepEqual(rerun, ['run', 'rerun', '42', '--failed']);
});

test('main in dry-run mode reports without re-running anything', async () => {
  const calls = [];
  const gh = async (args) => {
    calls.push(args);
    if (args[0] === 'run' && args[1] === 'list') return [run()];
    if (args[0] === 'pr' && args[1] === 'list') return [{ headRefName: 'feature-a', state: 'open' }];
    return [];
  };
  const code = await main(['--dry-run'], { gh, now: NOW });
  assert.equal(code, 0);
  assert.equal(calls.some((args) => args[0] === 'run' && args[1] === 'rerun'), false);
});

test('main fails when every selected rerun fails', async () => {
  const gh = async (args) => {
    if (args[0] === 'run' && args[1] === 'list') return [run()];
    if (args[0] === 'pr' && args[1] === 'list') return [{ headRefName: 'feature-a', state: 'open' }];
    if (args[0] === 'run' && args[1] === 'rerun') throw new Error('rerun rejected');
    return [];
  };
  const code = await main([], { gh, now: NOW });
  assert.equal(code, 1);
});
