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
// reviewed code is bound by a content tree that `completion()` compares with
// the receipt's, so an approval covers the code it was recorded for. The
// binding is the code and not the environment: the reviewer and the
// implementer do not share a machine, and an approval bound to an
// environment-bound fingerprint would not match a receipt produced
// elsewhere.
//
// Usage:
//   npm run harness -- review --verdict <APPROVE|REQUEST_CHANGES|BLOCKED>
//       --reviewer <name> --summary <text> [--finding <text>]...
//       [--findings-file <path>]
//   npm run harness -- review --show

import { appendFileSync, existsSync, lstatSync, readFileSync, readlinkSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { event, git, readJson, saveJson, statePath, taskAt } from './harness-state.mjs';

export const REVIEW_USAGE = 'review --verdict <APPROVE|REQUEST_CHANGES|BLOCKED> --reviewer <name> --summary <text> [--finding <text>]... [--findings-file <path>] | review --show';

const VERDICTS = ['APPROVE', 'REQUEST_CHANGES', 'BLOCKED'];

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
export function reviewContext(root, { fingerprint, changedPaths, contentTree, runIdentity, identityMismatch } = {}) {
  const task = taskAt(root);
  // With no active task there is no ledger to show: a worktree's rounds
  // belong to the task that recorded them.
  const rounds = task ? readRounds(root, task.id) : [];
  const last = rounds.at(-1) ?? null;
  const base = task?.base ?? null;
  const code = contentTree ? contentTree(root) : null;
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
          contentTree: last.contentTree ?? null,
          scope: last.scope ?? [],
        }
      : null,
    outstanding: outstandingFindings(rounds),
    repairScope: repairScope(root, last, changedPaths, base),
    currentTree: task?.base && fingerprint ? fingerprint(root, task.base) : null,
    // The code the last round read, and the code now. Equal means the
    // approval still covers the tree; different means the round reviewed
    // something else.
    currentContentTree: code,
    environment: last && last.identity && identityMismatch && runIdentity ? identityMismatch(last.identity, runIdentity(root, task?.base)) : [],
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

// The one rule for a review payload: the shape, and the verdict's own
// constraint. Both writers of a verdict use it — the `review` command and the
// `review` field of an `update --spec` — so a payload the update accepts is a
// payload the ledger accepts, and a payload the ledger refuses is refused
// before anything is written. Two copies of this rule are how a command ends
// up accepting one payload and refusing another.
//
// The rule owns the whole payload, including its absence: a caller passes what
// it has, and a payload that is null is a malformed payload rather than a
// crash. `prefix` names the field in the caller's own vocabulary, so the
// `review` command says `--verdict` and an update spec says `review.verdict`.
export function validateReview(payload = {}, { prefix = '' } = {}) {
  const { verdict, reviewer, summary, findings } = payload ?? {};
  const field = key => `${prefix}${key}`;
  const normalized = String(verdict ?? '').trim().toUpperCase();
  if (!VERDICTS.includes(normalized)) throw new Error(`${field('verdict')} must be one of ${VERDICTS.join(', ')}.`);
  const who = String(reviewer ?? '').trim();
  if (!who) throw new Error(`${field('reviewer')} is required.`);
  const note = String(summary ?? '').trim();
  if (!note) throw new Error(`${field('summary')} is required.`);
  const found = findings === undefined ? [] : findings;
  if (!Array.isArray(found) || found.some(item => typeof item !== 'string')) throw new Error(`${field('findings')} must be an array of strings.`);
  if (normalized === 'APPROVE' && found.length) throw new Error('An APPROVE cannot carry findings. Record REQUEST_CHANGES with the findings instead.');
  return { verdict: normalized, reviewer: who, summary: note, findings: found };
}

// Record one review round. The verdict lands in the same task field `update
// --spec` writes, and the reviewed code is bound by a content tree, so
// `completion()` still holds the guards it held before this command existed:
// the approval covers the code it was recorded for, whether or not the
// reviewer shared the implementer's environment.
export function recordReview(root, options, { fingerprint, changedPaths, contentTree, runIdentity, identityMismatch } = {}) {
  const task = taskAt(root);
  if (!task) throw new Error('BLOCKED: no active task. Start one with npm run harness -- start --spec <json>.');
  if (task.phase === 'complete') throw new Error('BLOCKED: the task is already complete.');
  if (!task.base) throw new Error('BLOCKED: the task has no base commit.');
  const { verdict, reviewer, summary, findings } = validateReview({ verdict: options.verdict, reviewer: options.reviewer, summary: options.summary, findings: readFindings(root, options) }, { prefix: '--' });

  const tree = fingerprint(root, task.base);
  const code = contentTree ? contentTree(root) : null;
  const identity = runIdentity ? runIdentity(root, task.base) : null;
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
    contentTree: code,
    identity,
    scope,
    contents: Object.fromEntries(scope.map(path => [path, contentHash(root, path)])),
  };

  // The round is appended before the verdict is written. The ledger is the
  // record of what was reviewed, so it must not be the thing that is missing
  // when the write is interrupted: a round without a task verdict blocks
  // completion with a verdict-shaped reason, while a verdict without a round
  // is the divergence this command exists to remove (issue #2630).
  appendFileSync(statePath(root, 'reviews.jsonl'), `${JSON.stringify(round)}\n`);
  task.review = { reviewer, summary, verdict, findings };
  task.reviewTree = tree;
  if (code) task.reviewContentTree = code;
  if (identity) task.reviewIdentity = identity;
  saveJson(statePath(root, 'active-task.json'), task);
  event(root, { type: 'review', taskId: task.id, round: round.round, reviewer, verdict, findings: findings.length, tree, head });

  return { recorded: round, context: reviewContext(root, { fingerprint, changedPaths, contentTree, runIdentity, identityMismatch }) };
}
