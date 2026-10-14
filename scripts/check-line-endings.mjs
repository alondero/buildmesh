#!/usr/bin/env node
// Line-ending invariant for the tracked index (issue #2044).
//
// Dependency-free so it can run before `npm ci`, like the other repo gates.
//
// The index is what a commit stores and what every later checkout is derived
// from, so it is the column worth gating. A worktree file still carrying
// pre-policy bytes is deliberately not a failure: `git status` compares the
// worktree through the same clean filter, so such a file is invisible to git,
// and any `git add` normalises it on the way in. Gating the worktree column
// instead would redden every checkout that predates the policy for a condition
// that cannot reach a commit; CONTRIBUTING.md owns the refresh command.
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

// Index kinds that are not a line-ending violation:
//
//   lf     the policy: every text file is stored with LF
//   -text  git found no text to convert (a PNG, a jar, an icon)
//   none   a file with no line ending to speak of (empty, or one unterminated line)
//   ''     a symlink, whose blob is the target path and has no worktree content
const ALLOWED_INDEX_KINDS = new Set(['lf', '-text', 'none', '']);

// `git ls-files --eol -z` emits one NUL-terminated record per tracked path:
//
//   i/lf    w/lf    attr/text=auto eol=lf \tsrc/lib/tauri.ts
//
// The status field never contains a tab, so the first one separates the path
// and a path containing a tab still parses.
export function parseEolRecords(text) {
  return text.split('\0').filter(Boolean).map((record) => {
    const separator = record.indexOf('\t');
    const fields = (separator === -1 ? record : record.slice(0, separator)).trim().split(/\s+/);
    const attributes = fields[2]?.startsWith('attr/') ? fields[2].slice(5) : '';
    return {
      index: fields[0]?.startsWith('i/') ? fields[0].slice(2) : '',
      worktree: fields[1]?.startsWith('w/') ? fields[1].slice(2) : '',
      // `-text` is the only binary marker in the attr column. It has to be read
      // rather than inferred from the kind: a binary file is never converted,
      // so git still reports the line endings it happens to contain, and the
      // fuzz corpus seeds are legitimately `i/crlf`.
      binary: attributes.split(/\s+/).includes('-text'),
      path: separator === -1 ? '' : record.slice(separator + 1),
    };
  });
}

export function checkLineEndings(cwd) {
  const records = execFileSync('git', ['ls-files', '--eol', '-z'], {
    cwd,
    encoding: 'utf8',
    maxBuffer: 32 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  return parseEolRecords(records)
    .filter((record) => !record.binary && !ALLOWED_INDEX_KINDS.has(record.index))
    .map((record) => {
      const what = record.index === 'mixed' ? 'mixed line endings' : `${record.index.toUpperCase()} line endings`;
      return `${record.path}: stored in the index with ${what} (i/${record.index}). `
        + 'The repository policy (issue #2044) stores every text file as LF; '
        + '`git add --renormalize .` re-stages the tree with LF.';
    });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    if (process.argv.length > 2) throw new Error('Usage: node scripts/check-line-endings.mjs');
    const failures = checkLineEndings(process.cwd());
    if (failures.length) {
      console.error(`Line-ending check failed (${failures.length} issue${failures.length === 1 ? '' : 's'}):`);
      for (const failure of failures) console.error(`  - ${failure}`);
      process.exitCode = 1;
    } else {
      console.log('Line endings passed (every tracked text file is stored LF in the index).');
    }
  } catch (error) {
    console.error(`Line-ending check failed: ${error.message}`);
    process.exitCode = 1;
  }
}
