// harness-state.mjs — the state helpers the harness and its review ledger share.
//
// The ledger lives in its own module so `harness.mjs` can import it, and the
// helpers both need live here so the two copies have one definition. A helper
// that exists twice is a helper whose two versions answer different questions
// one day apart, and these are the helpers that decide what the harness
// believes: where the task state is, what it contains, and what happened.
import { execFileSync } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { randomUUID } from 'node:crypto';

export const statePath = (root, name) => join(root, '.harness', name);
export const git = (root, ...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });
export const readJson = (path) => existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : null;
export function saveJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.${randomUUID()}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`);
  renameSync(temporary, path);
}
export function event(root, value) {
  mkdirSync(statePath(root, ''), { recursive: true });
  appendFileSync(statePath(root, 'events.jsonl'), `${JSON.stringify({ at: new Date().toISOString(), ...value })}\n`);
}
export function taskAt(root) {
  const task = readJson(statePath(root, 'active-task.json'));
  if (task && task.root !== root) throw new Error('BLOCKED: task belongs to a different worktree.');
  return task;
}
