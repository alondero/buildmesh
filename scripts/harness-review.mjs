// harness-review.mjs — the review-round ledger.
//
// A review round is a durable artefact, not a conversation. The harness already
// needed one for completion: `finish` requires an independent APPROVE recorded
// against the verified tree, and that verdict lives in the task state. What was
// missing was the other half — a round that a *follow-up* reviewer can pick up.
//
// On the gh2599 worktree (issue #2599) four review rounds ran over one change:
// 58, 60, 86 and 36 tool calls, each re-reading the whole diff, each re-running
// the 5,000-test unit suite by hand, three of them writing their own throwaway
// browser harness in %TEMP% because the repo had no owned way to assert the
// behaviour. The task then amended the same commit seven times and ended with
// `review: null` after twelve verification attempts, so `finish` had no verdict
// to check. The rounds had nowhere to land.
//
// This module owns that record. A round stores the verdict, the findings, the
// reviewed commit, the reviewed tree and a content hash per path. `review
// --show` reads the ledger back and adds the two things a follow-up reviewer
// would otherwise re-derive: the findings that are still outstanding, and the
// paths whose content changed since the last round reviewed.
//
// Recording a round does not weaken the completion guards. The verdict is
// written to the same `task.review` field `update --spec` writes, and the
// reviewed tree is the same fingerprint `finish` compares against the receipt:
// `completion()` refuses a task whose `reviewTree` is not the receipt's tree,
// so an approval covers the tree it was recorded for.
//
// Usage:
//   npm run harness -- review --verdict <APPROVE|REQUEST_CHANGES|BLOCKED>
//       --reviewer <name> --summary <text> [--finding <text>]...
//       [--findings-file <path>]
//   npm run harness -- review --show

import { execFileSync } from 'node:child_process';
import { appendFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readlinkSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';

export const REVIEW_USAGE = 'review --verdict <APPROVE|REQUEST_CHANGES|BLOCKED> --reviewer <name> --summary <text> [--finding <text>]... [--findings-file <path>] | review --show';

const VERDICTS = ['APPROVE', 'REQUEST_CHANGES', 'BLOCKED'];
const statePath = (root, name) => join(root, '.harness', name);
const git = (root, ...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });
const readJson = (path) => (existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : null);

// A path's content at a point in time. The repair scope is a difference of
// content, not of commits: a review usually lands on an uncommitted tree, so
// comparing against the reviewed commit would report the whole change again.
// A symlink is hashed by its target, the way the harness's own fingerprint
// hashes one, so the two agree on what "this path changed" means.
function contentHash(root, path) {
  try {
    const stat = lstatSync(join(root, path));
    if (stat.isSymbolicLink()) return `link:${readlinkSync(join(root, path))}`;
    if (!stat.isFile()) return 'not-a-file';
    return createHash('sha256').update(readFileSync(join(root, path))).digest('hex');
  } catch {
    return 'absent';
  }
}

function saveJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.${randomUUID()}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`);
  renameSync(temporary, path);
}

function event(root, value) {
  mkdirSync(statePath(root, ''), { recursive: true });
  appendFileSync(statePath(root, 'events.jsonl'), `${JSON.stringify({ at: new Date().toISOString(), ...value })}\n`);
}

function taskAt(root) {
  const task = readJson(statePath(root, 'active-task.json'));
  if (task && task.root !== root) throw new Error('BLOCKED: task belongs to a different worktree.');
  return task;
}

// The ledger is append-only: a round is history, and rewriting history is how a
// dropped finding comes back as an "unresolved" one. Rounds belong to the task
// that recorded them, so a new task in the same worktree does not inherit the
// previous task's verdict.
export function readRounds(root, taskId = null) {
  const path = statePath(root, 'reviews.jsonl');
  if (!existsSync(path)) return [];
  const rounds = readFileSync(path, 'utf8').split('\n').filter(Boolean).map(line => {
    try { return JSON.parse(line); } catch { return null; }
  }).filter(Boolean);
  return taskId ? rounds.filter(round => round.taskId === taskId) : rounds;
}

// A finding survives until an approval clears it. A later round adds to the
// backlog rather than replacing it, so the repairs for the first round's
// findings do not fall out of the record when the second round reports a
// different set. A BLOCKED round names the missing prerequisite, so its
// findings are outstanding too.
export function outstandingFindings(rounds) {
  const findings = [];
  for (const round of [...rounds].reverse()) {
    if (round.verdict === 'APPROVE') break;
    if (round.verdict === 'REQUEST_CHANGES' || round.verdict === 'BLOCKED') findings.push(...(round.findings ?? []));
  }
  return [...new Set(findings)];
}

// The paths whose content changed since the last round reviewed. The round
// stored a content hash per path, so the scope is the difference between the
// tree the reviewer saw and the tree now — not the difference between two
// commits, which would report the whole change whenever the review landed on
// an uncommitted tree.
function repairScope(root, last, changedPaths, base) {
  if (!last) return [];
  const stored = last.contents ?? {};
  const current = new Set(changedPaths && base ? changedPaths(root, base) : []);
  const paths = new Set([...Object.keys(stored), ...current]);
  return [...paths]
    .filter(path => (stored[path] ?? 'absent') !== contentHash(root, path))
    .sort();
}

// Everything a follow-up reviewer needs, in one read: what the last round
// concluded, what is still outstanding, what changed since, and which command
// to run. The scoped check is named here because the expensive default (the
// full suite, run by hand) is what the gh2599 rounds actually did.
export function reviewContext(root, { fingerprint, changedPaths } = {}) {
  const task = taskAt(root);
  // With no active task there is no ledger to show: a worktree's rounds
  // belong to the task that recorded them.
  const rounds = task ? readRounds(root, task.id) : [];
  const last = rounds.at(-1) ?? null;
  const base = task?.base ?? null;
  return {
    taskId: task?.id ?? null,
    base,
    rounds: rounds.length,
    last: last
      ? {
          at: last.at,
          reviewer: last.reviewer,
          verdict: last.verdict,
          summary: last.summary,
          findings: last.findings ?? [],
          head: last.head,
          tree: last.tree,
          scope: last.scope ?? [],
        }
      : null,
    outstanding: outstandingFindings(rounds),
    repairScope: repairScope(root, last, changedPaths, base),
    currentTree: task?.base && fingerprint ? fingerprint(root, task.base) : null,
    commands: {
      scopedCheck: base ? `npm run harness -- check --base ${base}` : 'npm run harness -- check --base <base>',
      verify: 'npm run verify',
    },
  };
}

function readFindings(root, options) {
  const findings = [];
  for (const finding of options.finding ?? []) {
    const text = String(finding).trim();
    if (text) findings.push(text);
  }
  if (options.findingsFile) {
    const path = resolve(root, options.findingsFile);
    if (!existsSync(path)) throw new Error(`Findings file not found: ${path}`);
    for (const line of readFileSync(path, 'utf8').split('\n')) {
      const text = line.replace(/\r$/, '').trim();
      if (text) findings.push(text);
    }
  }
  return [...new Set(findings)];
}

// Record one review round. The verdict lands in the same task field `update
// --spec` writes, and the reviewed tree is the same fingerprint `finish`
// compares against the receipt, so `completion()` still holds the guards it
// held before this command existed.
export function recordReview(root, options, { fingerprint, changedPaths } = {}) {
  const task = taskAt(root);
  if (!task) throw new Error('BLOCKED: no active task. Start one with npm run harness -- start --spec <json>.');
  if (task.phase === 'complete') throw new Error('BLOCKED: the task is already complete.');
  if (!task.base) throw new Error('BLOCKED: the task has no base commit.');
  const verdict = String(options.verdict ?? '').toUpperCase();
  if (!VERDICTS.includes(verdict)) throw new Error(`--verdict must be one of ${VERDICTS.join(', ')}.`);
  const reviewer = String(options.reviewer ?? '').trim();
  if (!reviewer) throw new Error('--reviewer is required.');
  const summary = String(options.summary ?? '').trim();
  if (!summary) throw new Error('--summary is required.');
  const findings = readFindings(root, options);
  if (verdict === 'APPROVE' && findings.length) throw new Error('An APPROVE cannot carry findings. Record REQUEST_CHANGES with the findings instead.');

  const tree = fingerprint(root, task.base);
  const head = git(root, 'rev-parse', 'HEAD').trim();
  const scope = changedPaths ? changedPaths(root, task.base) : [];
  const round = {
    at: new Date().toISOString(),
    round: readRounds(root, task.id).length + 1,
    taskId: task.id,
    reviewer,
    verdict,
    summary,
    findings,
    base: task.base,
    head,
    tree,
    scope,
    contents: Object.fromEntries(scope.map(path => [path, contentHash(root, path)])),
  };

  task.review = { reviewer, summary, verdict, findings };
  task.reviewTree = tree;
  saveJson(statePath(root, 'active-task.json'), task);
  appendFileSync(statePath(root, 'reviews.jsonl'), `${JSON.stringify(round)}\n`);
  event(root, { type: 'review', taskId: task.id, round: round.round, reviewer, verdict, findings: findings.length, tree, head });

  return { recorded: round, context: reviewContext(root, { fingerprint, changedPaths }) };
}
