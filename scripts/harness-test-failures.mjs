import { existsSync, readFileSync } from 'node:fs';
import { isAbsolute, relative } from 'node:path';
import { stripVTControlCharacters } from 'node:util';

export const gatePassed = row => row?.outcome === 'PASS' || (row?.outcome === 'FLAKY' && row.failures?.length > 0 && row.failures.every(test => test.outcome === 'FLAKY' && Number.isInteger(test.issue)));

export function knownFlakes(root) {
  let entries;
  try { entries = JSON.parse(readFileSync(`${root}/scripts/known-flakes.json`, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return {}; throw error; }
  if (!entries || Array.isArray(entries) || typeof entries !== 'object') throw new Error('known-flakes.json must map exact test ids to issue numbers.');
  for (const [id, issue] of Object.entries(entries)) {
    if (!id.trim() || !Number.isInteger(issue) || issue < 1) throw new Error(`Invalid known-flake entry: ${id}`);
  }
  return entries;
}

export async function validateFlakeIssues(entries, loadIssue = async number => {
  const token = process.env.GH_TOKEN ?? process.env.GITHUB_TOKEN;
  const response = await fetch(`https://api.github.com/repos/alondero/buildmesh/issues/${number}`, {
    headers: { Accept: 'application/vnd.github+json', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
    signal: AbortSignal.timeout(10000),
  });
  if (!response.ok) throw new Error(`Cannot check known-flake issue #${number}: HTTP ${response.status}`);
  return response.json();
}) {
  for (const number of new Set(Object.values(entries))) {
    const issue = await loadIssue(number);
    if (issue.state !== 'open' || issue.pull_request) throw new Error(`Known-flake issue #${number} must be an open issue.`);
  }
}

export async function conptyPrerequisite(root, probe = fetch) {
  const { VERSION } = await import('./prepare-conpty.mjs');
  if (existsSync(`${root}/src-tauri/target/conpty/${VERSION}/package.zip`)) return null;
  const url = `https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/${VERSION}/microsoft.windows.console.conpty.${VERSION}.nupkg`;
  try {
    const response = await probe(url, { method: 'HEAD', signal: AbortSignal.timeout(10000) });
    if (response.ok) return null;
    return `Windows ConPTY package is not cached and its download is unavailable (HTTP ${response.status}); restore network access to api.nuget.org.`;
  } catch {
    return 'Windows ConPTY package is not cached and api.nuget.org is unreachable; restore network access before compiling Rust.';
  }
}

export function vitestReport(root, report, output = '') {
  const failures = [];
  let unattributed = /Unhandled (?:Error|Rejection)|Uncaught Exception|Errors\s+[1-9]/i.test(stripVTControlCharacters(output));
  for (const suite of report.testResults ?? []) {
    const file = (isAbsolute(suite.name) ? relative(root, suite.name) : suite.name).replaceAll('\\', '/');
    const failed = (suite.assertionResults ?? []).filter(test => test.status === 'failed');
    if (suite.status === 'failed' && (!failed.length || suite.message)) unattributed = true;
    if (suite.status === 'failed' && !failed.length) failures.push({ id: file, file, name: null });
    for (const test of failed) failures.push({ id: `${file} > ${test.fullName}`, file, name: test.fullName });
  }
  return { failures, count: report.numPassedTests ?? 0, unattributed: unattributed || failures.length !== report.numFailedTests, report };
}

export function rustFailures(output, target = { kind: 'lib' }) {
  const text = stripVTControlCharacters(output);
  // The first failures block contains panic details; the second contains ids.
  const names = [...text.matchAll(/^test (.+) \.\.\. FAILED\s*$/gm)].map(match => match[1]);
  for (const block of text.matchAll(/^failures:\r?\n\r?\n((?: {4}.+\r?\n)+)\r?\n(?=test result:)/gm)) {
    names.push(...block[1].trim().split(/\r?\n/).map(line => line.trim()));
  }
  return [...new Set(names)].map(name => ({ id: target.kind === 'lib' ? name : `${target.kind}:${target.name} > ${name}`, name, target }));
}

export function readTestReport(root, kind, path, output) {
  let report;
  try { report = JSON.parse(readFileSync(path, 'utf8')); } catch { /* A crashed reporter is a failure, never green. */ }
  if (kind === 'vitest') return report ? vitestReport(root, report, output) : { failures: [], count: 0, unattributed: true };
  if (report) return report;
  // Also support a plain cargo test gate. The shard runner supplies target metadata.
  const text = stripVTControlCharacters(output);
  const doc = text.match(/^\s*Doc-tests (\S+)/m);
  const failures = doc ? [...rustFailures(text.slice(0, doc.index)), ...rustFailures(text.slice(doc.index), { kind: 'doc', name: doc[1] })] : rustFailures(text);
  const failed = [...text.matchAll(/test result: FAILED\. \d+ passed; (\d+) failed/g)].reduce((sum, match) => sum + Number(match[1]), 0);
  const count = [...text.matchAll(/test result: (?:ok|FAILED)\. (\d+) passed/g)].reduce((sum, match) => sum + Number(match[1]), 0);
  return { failures, count, unattributed: !failed || failed !== failures.length };
}

const escapeRegex = text => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
export function isolatedCommand(test, kind, reportPath) {
  if (kind === 'vitest') return ['node', 'scripts/harness-vitest-isolate.mjs', test.file, `^${escapeRegex(test.name)}$`, reportPath];
  const { kind: targetKind, name } = test.target;
  if (targetKind === 'doc') throw new Error('Doctest isolation unavailable; no reliable exact selector.');
  const target = targetKind === 'lib' ? ['--lib'] : [`--${targetKind}`, name];
  return ['cargo', 'test', '--locked', ...target, test.name, '--', '--exact', '--test-threads=1'];
}

// A shared budget starts at the first diagnosis, after both product lanes drain.
export async function isolateTests(root, original, run, entries = knownFlakes(root), budget = {}) {
  const now = budget.now ?? Date.now;
  budget.deadline ??= now() + 10 * 60 * 1000;
  const failures = [];
  for (const test of original.failures) {
    if (test.target?.kind === 'doc') {
      failures.push({ ...test, outcome: 'FAIL', reason: 'Doctest isolation unavailable: rustdoc cannot reliably select one exact test; see the original log.' });
      continue;
    }
    if (!test.name) {
      failures.push({ ...test, outcome: 'FAIL', reason: 'File collection/setup failed; no exact test selector is available.' });
      continue;
    }
    const remainingMs = budget.deadline - now();
    if (remainingMs <= 0) {
      failures.push({ ...test, outcome: 'TIMEOUT', reason: 'Total isolated-rerun budget exhausted; test was not isolated.' });
      continue;
    }
    const rerun = await run(test, remainingMs);
    const passed = rerun.exitCode === 0 && rerun.count > 0 && rerun.matched !== false;
    const outcome = passed ? 'FLAKY' : rerun.exitCode === 124 ? 'TIMEOUT' : rerun.exitCode === 127 ? 'BLOCKED' : 'FAIL';
    failures.push({ ...test, outcome, ...(rerun.reason ? { reason: rerun.reason } : {}), ...(passed && Object.hasOwn(entries, test.id) ? { issue: entries[test.id] } : {}), rerun });
  }
  const outcome = original.unattributed || failures.some(test => test.outcome === 'FAIL') ? 'FAIL'
    : failures.find(test => test.outcome === 'TIMEOUT' || test.outcome === 'BLOCKED')?.outcome ?? 'FLAKY';
  const row = { outcome, failures };
  return { ...row, reason: outcome === 'FLAKY' ? (gatePassed(row)
    ? 'Known flakes passed alone; linked open issues permit completion.'
    : 'Tests passed alone. File an issue and add exact ids to scripts/known-flakes.json; unlisted flakes block finish.')
    : original.unattributed ? 'Additional suite/runtime failures remain; see the original log.' : failures.find(test => test.reason)?.reason ?? 'Isolated rerun did not pass; see each test rerun log.' };
}

export function failureLines(row, limit = 10) {
  const lines = (row.failures ?? []).slice(0, limit).map(test => `  ${test.outcome ?? 'FAIL'} ${test.id}${test.issue ? ` https://github.com/alondero/buildmesh/issues/${test.issue}` : ''}${test.reason ? `: ${test.reason}` : ''}${test.rerun?.log ? ` (rerun: ${test.rerun.log})` : ''}`);
  if (row.failures?.length > limit) lines.push(`  (+${row.failures.length - limit} more failing tests in the receipt)`);
  return lines;
}
