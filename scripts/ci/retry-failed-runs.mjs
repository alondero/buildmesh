#!/usr/bin/env node
// retry-failed-runs.mjs — one automatic re-run for Build runs that failed or
// were cancelled, run on a schedule by .github/workflows/ci-retry.yml.
//
// Diagnosis behind this (the "hours to mergeable" investigation): a healthy
// verification run is 10-13 minutes, yet pull requests took hours to reach a
// mergeable state partly because flaky infra failures sat until a human or an
// agent noticed and re-ran them — 4 of 60 recent runs needed a second attempt,
// and every second attempt in the window was triggered manually.
//
// A re-run is only worth doing when it is fresh and still relevant, so a run
// qualifies only if all of these hold:
//
//   - it completed with conclusion `failure` or `cancelled`;
//   - it is still on its first attempt (one automatic retry, never two);
//   - it is less than `windowMs` old (default six hours);
//   - it is the newest run for its event + head branch — a newer run renders
//     an older failure stale the same way the workflow's concurrency group
//     cancels superseded work;
//   - for pull-request runs, the pull request is still open.
//
// `gh run rerun <id> --failed` re-runs only the failed jobs of a run, so a
// blip in one shard costs that shard, not the whole fan-out.
//
// Usage:
//   node scripts/ci/retry-failed-runs.mjs [--dry-run]
//   GH_TOKEN=... (the scheduled workflow provides this)

import { spawn } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const USAGE = 'Usage: node scripts/ci/retry-failed-runs.mjs [--dry-run]';
const DEFAULT_WINDOW_MS = 6 * 60 * 60 * 1000;
const DEFAULT_MAX_RETRIES = 3;
const LIST_LIMIT = 100;

export function normaliseBranch(branch) {
  return String(branch ?? '').replace(/^refs\/heads\//, '');
}

export function isPrStateRetryable(prStateByBranch, branch) {
  const state = prStateByBranch.get(normaliseBranch(branch));
  return String(state ?? '').toLowerCase() === 'open';
}

export function selectRunsToRetry(runs, { now = Date.now(), windowMs = DEFAULT_WINDOW_MS, maxRetries = DEFAULT_MAX_RETRIES, prStateByBranch = new Map() } = {}) {
  const keyOf = (r) => `${r.event ?? ''} ${normaliseBranch(r.headBranch)}`;
  const newestByKey = new Map();
  for (const r of runs) {
    const at = Date.parse(r.createdAt);
    if (Number.isNaN(at)) continue;
    const key = keyOf(r);
    const previous = newestByKey.get(key);
    if (!previous || at > previous.at) newestByKey.set(key, { at, run: r });
  }

  const candidates = runs.filter((r) => {
    if (r.status !== 'completed') return false;
    if (r.conclusion !== 'failure' && r.conclusion !== 'cancelled') return false;
    if ((r.attempt ?? 1) > 1) return false;
    const at = Date.parse(r.createdAt);
    if (Number.isNaN(at) || at < now - windowMs) return false;
    if (newestByKey.get(keyOf(r))?.run?.databaseId !== r.databaseId) return false;
    if (r.event === 'pull_request' && !isPrStateRetryable(prStateByBranch, r.headBranch)) return false;
    return true;
  });

  candidates.sort((a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt));
  return candidates.slice(0, maxRetries);
}

export function ghJson(args) {
  return new Promise((resolve, reject) => {
    const child = spawn('gh', args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('error', reject);
    child.on('close', (code) => {
      if (code !== 0) {
        reject(new Error(`gh ${args.join(' ')} exited ${code}: ${stderr.trim()}`));
        return;
      }
      try {
        resolve(JSON.parse(stdout));
      } catch (err) {
        reject(new Error(`gh ${args.join(' ')} returned invalid JSON: ${err.message}`));
      }
    });
  });
}

export async function main(argv, { gh = ghJson, now = Date.now() } = {}) {
  const dryRun = argv.includes('--dry-run');
  if (argv.some((arg) => arg !== '--dry-run')) {
    process.stderr.write(`${USAGE}\n`);
    return 2;
  }

  const runs = await gh([
    'run', 'list', '--workflow', 'build.yml', '--limit', String(LIST_LIMIT),
    '--json', 'databaseId,status,conclusion,event,headBranch,createdAt,attempt,url',
  ]);

  const prBranches = new Set(
    runs.filter((r) => r.event === 'pull_request').map((r) => normaliseBranch(r.headBranch)),
  );
  const prStateByBranch = new Map();
  if (prBranches.size > 0) {
    // Open only: `--state all` spends the limit on dead pull requests, so a
    // still-open branch can fall outside the window, read as missing, and
    // never be retried (review of PR #1991). A missing entry already means
    // "not open" to isPrStateRetryable, so closed/merged pull requests stay
    // excluded without being fetched.
    const prs = await gh(['pr', 'list', '--state', 'open', '--limit', '200', '--json', 'headRefName,state']);
    for (const pr of prs) {
      const branch = normaliseBranch(pr.headRefName);
      if (!prBranches.has(branch)) continue;
      // A reused branch name can appear on several pull requests; keep the
      // open verdict so the retry decision does not depend on GitHub's
      // response order.
      if (String(prStateByBranch.get(branch) ?? '').toLowerCase() === 'open') continue;
      prStateByBranch.set(branch, pr.state);
    }
  }

  const selected = selectRunsToRetry(runs, { now, prStateByBranch });
  if (selected.length === 0) {
    console.log('ci-retry: no failed Build runs need a re-run.');
    return 0;
  }

  let failures = 0;
  for (const run of selected) {
    console.log(`ci-retry: ${dryRun ? 'would re-run' : 're-running'} failed jobs of ${run.url} (${run.event} ${normaliseBranch(run.headBranch)}, attempt ${run.attempt})`);
    if (dryRun) continue;
    try {
      await gh(['run', 'rerun', String(run.databaseId), '--failed']);
    } catch (err) {
      failures += 1;
      console.error(`ci-retry: rerun of ${run.url} failed: ${err.message}`);
    }
  }
  return failures > 0 && failures === selected.length ? 1 : 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  }).catch((err) => {
    console.error(`ci-retry: ${err.message}`);
    process.exitCode = 1;
  });
}
