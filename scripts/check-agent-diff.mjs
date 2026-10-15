#!/usr/bin/env node
// Reuse Claude's content rules for shell edits, other agents, and CI.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { checkContentViolations } from '../.claude/hooks/guard-antipatterns.mjs';
import { checkLineEndings } from './check-line-endings.mjs';

// git's stderr is captured rather than inherited, the way `harness.mjs`,
// `self-review.mjs` and `check-line-endings.mjs` already capture theirs: the CLI
// reports a failure through `error.message`, which carries the captured stderr,
// so an unresolvable base no longer prints a bare `fatal: Needed a single
// revision` into a log that now carries on to the next step (issue #2299).
const gitIn = cwd => (...args) => execFileSync('git', args, {
  cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'],
});

// An all-zero sha is the "this ref was created" sentinel GitHub sends, and an
// empty base is a caller that never filled the value in; both mean "there is no
// earlier commit to compare against", which is what `HEAD` says. Same candidate
// rule as `scripts/check-docs.mjs`, so the pair cannot disagree about one input.
function candidateBase(base) {
  const requested = String(base ?? '').trim();
  return !requested || /^0+$/.test(requested) ? 'HEAD' : requested;
}

// Resolving the base is a separate step from diffing against it, because the
// two answers differ in kind: a base that resolves is a usable left edge, and
// one that does not is a *missing signal* the caller has to be told about
// (issue #2299). On a tag push `github.event.before` is the previous annotated
// tag object, and a force-moved tag means the runner's clone does not have it.
function resolveBase(cwd, base = 'HEAD') {
  const candidate = candidateBase(base);
  try {
    return {
      base: candidate,
      commit: gitIn(cwd)('rev-parse', '--verify', '--end-of-options', `${candidate}^{commit}`).trim(),
      skipped: false,
    };
  } catch (error) {
    return { base: candidate, skipped: true, reason: error.message };
  }
}

export function checkDiff(cwd, base = 'HEAD') {
  const resolved = resolveBase(cwd, base);
  // A missing base must fail here, rather than report an empty successful
  // check. `checkAgentRules` is the gate the CLI runs for `npm run check:agent`,
  // the harness fast gate and CI, and it is what decides what an unavailable
  // base means; this is the primitive behind it.
  if (resolved.skipped) throw new Error(`diff base "${resolved.base}" does not resolve: ${resolved.reason}`);
  return checkDiffFrom(cwd, resolved.commit);
}

function checkDiffFrom(cwd, commit) {
  const git = gitIn(cwd);
  const paths = ['src', 'src-tauri', 'scripts'];
  const changed = git('diff', '--no-renames', '--name-only', '-z', '--diff-filter=ACMRT', commit, '--', ...paths).split('\0').filter(Boolean);
  const untracked = new Set(git('ls-files', '--others', '--exclude-standard', '-z', '--', ...paths).split('\0').filter(Boolean));
  const failures = [];
  for (const file of new Set([...changed, ...untracked])) {
    // `.ps1` is here for the PowerShell condition rule (#1982): a wrong shell
    // check is silent at runtime, so the gate has to see it outside the hooks.
    if (!/\.(?:tsx?|jsx?|rs|ps1)$/i.test(file)) continue;
    const text = untracked.has(file)
      ? readFileSync(resolve(cwd, file), 'utf8')
      : git('diff', '--no-renames', '--no-ext-diff', '--no-textconv', '--unified=0', commit, '--', file)
        .split('\n').filter(line => line.startsWith('+') && !line.startsWith('+++')).map(line => line.slice(1)).join('\n');
    for (const message of checkContentViolations(file, text)) failures.push(`${file}: ${message}`);
  }
  return failures;
}

// The diff-scoped content rules above only see files this change touched. The
// line-ending invariant is repository-wide and has teeth against exactly the
// edit that renorms a file, so it runs here, on the same CLI every entry point
// already calls (`npm run check:agent`, the harness fast gate, CI).
//
// The two halves do not need the same things: the content rules need a diff
// base, the line-ending invariant reads only the index. So an unresolvable base
// costs the first half and not the second. That is the whole point — the same
// missing signal is already advisory in the documentation gate, and failing the
// job here reddened a release run with nothing to report, whose `release` job
// then never started (issue #2299). The caller is told, so the reduced coverage
// is visible rather than a silent pass.
export function checkAgentRules(cwd, base = 'HEAD') {
  const resolved = resolveBase(cwd, base);
  return {
    failures: [...(resolved.skipped ? [] : checkDiffFrom(cwd, resolved.commit)), ...checkLineEndings(cwd)],
    base: resolved.base,
    skipped: resolved.skipped,
    reason: resolved.reason,
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const args = process.argv.slice(2);
    if (args.length && (args.length !== 2 || args[0] !== '--base')) throw new Error('Usage: node scripts/check-agent-diff.mjs [--base <commit>]');
    const result = checkAgentRules(process.cwd(), args[1]);
    if (result.skipped) {
      console.warn(
        `::warning::Changed-code scan skipped: diff base '${result.base}' does not resolve in this checkout. `
        + 'Only the repository-wide line-ending invariant ran; the content rules did not.',
      );
    }
    if (result.failures.length) {
      console.error(result.failures.join('\n\n'));
      process.exitCode = 1;
    } else if (result.skipped) {
      console.log('Line endings passed; the changed-code scan was skipped because its diff base is unavailable.');
    } else console.log('Agent diff rules and the line-ending policy passed (content heuristics; see docs/agents/engineering.md for limits).');
  } catch (error) {
    console.error(`Agent diff check failed: ${error.message}`);
    process.exitCode = 1;
  }
}
