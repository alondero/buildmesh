#!/usr/bin/env node
// Reuse Claude's content rules for shell edits, other agents, and CI.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { checkContentViolations } from '../.claude/hooks/guard-antipatterns.mjs';

export function checkDiff(cwd, base = 'HEAD') {
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  // A missing base must fail, rather than report an empty successful check.
  const commit = git('rev-parse', '--verify', '--end-of-options', `${base}^{commit}`).trim();
  const paths = ['src', 'src-tauri'];
  const changed = git('diff', '--no-renames', '--name-only', '-z', '--diff-filter=ACMRT', commit, '--', ...paths).split('\0').filter(Boolean);
  const untracked = new Set(git('ls-files', '--others', '--exclude-standard', '-z', '--', ...paths).split('\0').filter(Boolean));
  const failures = [];
  for (const file of new Set([...changed, ...untracked])) {
    if (!/\.(?:tsx?|jsx?|rs)$/.test(file)) continue;
    const text = untracked.has(file)
      ? readFileSync(resolve(cwd, file), 'utf8')
      : git('diff', '--no-renames', '--no-ext-diff', '--no-textconv', '--unified=0', commit, '--', file)
        .split('\n').filter(line => line.startsWith('+') && !line.startsWith('+++')).map(line => line.slice(1)).join('\n');
    for (const message of checkContentViolations(file, text)) failures.push(`${file}: ${message}`);
  }
  return failures;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const args = process.argv.slice(2);
    if (args.length && (args.length !== 2 || args[0] !== '--base')) throw new Error('Usage: node scripts/check-agent-diff.mjs [--base <commit>]');
    const failures = checkDiff(process.cwd(), args[1]);
    if (failures.length) {
      console.error(failures.join('\n\n'));
      process.exitCode = 1;
    } else console.log('Agent diff rules passed (content heuristics; see docs/agents/engineering.md for limits).');
  } catch (error) {
    console.error(`Agent diff check failed: ${error.message}`);
    process.exitCode = 1;
  }
}
