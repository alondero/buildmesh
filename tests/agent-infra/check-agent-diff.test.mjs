import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkAgentRules, checkDiff } from '../../scripts/check-agent-diff.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const hook = join(root, '.claude/hooks/guard-antipatterns.mjs');
const checker = join(root, 'scripts/check-agent-diff.mjs');

function repo(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'buildmesh-agent-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  git('init', '-q');
  git('config', 'user.name', 'Agent test');
  git('config', 'user.email', 'agent-test@example.invalid');
  git('config', 'core.autocrlf', 'false');
  git('config', 'core.hooksPath', join(cwd, 'no-hooks'));
  mkdirSync(join(cwd, 'src/components'), { recursive: true });
  const put = (file, text) => writeFileSync(join(cwd, file), text);
  put('src/components/Terminal.ts', 'const original = true;\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'baseline');
  return { cwd, git, put };
}

test('catches staged, unstaged and untracked violations without changing the index', t => {
  const { cwd, git, put } = repo(t);
  put('src/components/Terminal.ts', 'terminal.dispose();\n');
  git('add', '.');
  put('src/components/Terminal.ts', 'terminal.dispose();\nconst radius = "rounded";\n');
  put('src/components/NewTerminal.ts', 'terminal.dispose();\n');
  const before = git('diff', '--cached');
  const failures = checkDiff(cwd);
  assert.equal(failures.length, 3);
  assert.ok(failures.some(f => f.includes('NewTerminal.ts')));
  assert.equal(git('diff', '--cached'), before);
});

test('checks committed changes against the requested base', t => {
  const { cwd, git, put } = repo(t);
  const base = git('rev-parse', 'HEAD').trim();
  put('src/components/Terminal.ts', 'terminal.dispose();\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'change');
  assert.deepEqual(checkDiff(cwd), []);
  assert.equal(checkDiff(cwd, base).length, 1);
  // The strict primitive still refuses an unusable base rather than reporting
  // an empty successful check; `checkAgentRules` is what degrades to a warning.
  assert.throws(() => checkDiff(cwd, 'missing-base'), /does not resolve/);
});

test('an unresolvable base warns, skips only the content scan, and passes (issue #2299)', t => {
  const { cwd, git, put } = repo(t);
  const base = git('rev-parse', 'HEAD').trim();
  put('src/components/Terminal.ts', 'terminal.dispose();\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'change');

  // The force-updated-tag push: `github.event.before` is an annotated tag object
  // the runner's clone does not have. The missing signal warns, it does not fail.
  const unavailable = checkAgentRules(cwd, 'e9e059dccd4a8e20ed5673d55281525dc3c7f314');
  assert.equal(unavailable.skipped, true);
  assert.equal(unavailable.base, 'e9e059dccd4a8e20ed5673d55281525dc3c7f314');
  assert.deepEqual(unavailable.failures, []);
  assert.equal(checkAgentRules(cwd, base).skipped, false);

  const run = spawnSync(process.execPath, [checker, '--base', 'missing-base'], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 0, run.stderr);
  assert.match(run.stderr, /::warning::Changed-code scan skipped/);
  assert.match(run.stderr, /missing-base/);
  assert.match(run.stdout, /skipped because its diff base is unavailable/);

  // Skipping the content rules must not skip the repository-wide invariant,
  // which reads the index and needs no base: the fallback still fails for a
  // CRLF commit, and still names the violation.
  mkdirSync(join(cwd, 'src/lib'), { recursive: true });
  put('src/lib/staged.ts', 'export const a = 1;\r\n');
  git('add', 'src/lib/staged.ts');
  const skippedButCaught = spawnSync(process.execPath, [checker, '--base', 'missing-base'], { cwd, encoding: 'utf8' });
  assert.equal(skippedButCaught.status, 1);
  assert.match(skippedButCaught.stderr, /staged\.ts: stored in the index with CRLF line endings/);
});

test('a resolvable base still fails the changed-code scan on a violation', t => {
  const { cwd, git, put } = repo(t);
  const base = git('rev-parse', 'HEAD').trim();
  put('src/components/Terminal.ts', 'terminal.dispose();\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'change');
  const result = checkAgentRules(cwd, base);
  assert.equal(result.skipped, false);
  assert.equal(result.failures.length, 1);
  assert.match(result.failures[0], /Terminal\.ts.*terminal-dispose/);
  const run = spawnSync(process.execPath, [checker, '--base', base], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 1);
  assert.match(run.stderr, /terminal-dispose/);
  assert.doesNotMatch(run.stderr, /::warning::/);
});

test('an empty or all-zero base is the HEAD sentinel, not an unavailable one', t => {
  const { cwd, git, put } = repo(t);
  const base = git('rev-parse', 'HEAD').trim();
  put('src/components/Terminal.ts', 'terminal.dispose();\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'change');
  for (const sentinel of ['', '0000000000000000000000000000000000000000']) {
    const result = checkAgentRules(cwd, sentinel);
    assert.equal(result.skipped, false, sentinel);
    assert.equal(result.base, 'HEAD', sentinel);
    assert.equal(result.failures.length, 0, sentinel);
  }
});

test('allows unchanged legacy code, deletions, reasoned exceptions, and paths with spaces', t => {
  const { cwd, git, put } = repo(t);
  put('src/components/OldTerminal.ts', 'terminal.dispose();\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'legacy');
  put('src/components/OldTerminal.ts', 'const removed = true;\n');
  put('src/components/New Terminal.ts', 'terminal.dispose(); // allow-dispose: node deleted\n');
  assert.deepEqual(checkDiff(cwd), []);
  const run = spawnSync(process.execPath, [checker], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 0, run.stderr);
  assert.match(run.stdout, /passed/);
});

test('hook executable denies bad edits and accepts safe or malformed payloads', () => {
  const run = input => spawnSync(process.execPath, [hook], { input, encoding: 'utf8' });
  const denied = run(JSON.stringify({ tool_input: { file_path: 'src/components/Terminal.ts', new_string: 'term.dispose();' } }));
  assert.equal(denied.status, 2);
  assert.match(denied.stderr, /terminal-dispose/);
  assert.equal(run(JSON.stringify({ tool_input: { file_path: 'src/components/Terminal.ts', new_string: 'term.write("ok");' } })).status, 0);
  assert.equal(run('not JSON').status, 0);
});

test('a rename into a guarded path is checked even without content changes', t => {
  const { cwd, git, put } = repo(t);
  put('src/utility.ts', 'terminal.dispose();\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'unguarded path');
  git('mv', 'src/utility.ts', 'src/components/Renamed Terminal.ts');
  const failures = checkDiff(cwd);
  assert.equal(failures.length, 1);
  assert.match(failures[0], /Renamed Terminal.ts.*terminal-dispose/);
});

test('flags a PowerShell condition on a native command in scripts/*.ps1 (#1982)', t => {
  const { cwd, git, put } = repo(t);
  mkdirSync(join(cwd, 'scripts'), { recursive: true });
  put('scripts/check-merge.ps1', 'Write-Output "probe"\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'scripts baseline');
  assert.deepEqual(checkDiff(cwd), []);

  // The silent, always-false form must be reported by the CI-facing gate too,
  // not only by the Claude PreToolUse hook.
  put(
    'scripts/check-merge.ps1',
    'Write-Output "probe"\nif (git diff --quiet) { Write-Output "unchanged" }\n',
  );
  const failures = checkDiff(cwd);
  assert.equal(failures.length, 1, failures.join('\n'));
  assert.match(failures[0], /scripts\/check-merge\.ps1.*powershell-if-native-command/);

  // And the $LASTEXITCODE form is not flagged.
  put(
    'scripts/check-merge.ps1',
    'Write-Output "probe"\ngit diff --quiet\nif ($LASTEXITCODE -eq 0) { Write-Output "unchanged" }\n',
  );
  assert.deepEqual(checkDiff(cwd), []);

  // CRLF is the normal shape of a `.ps1` payload: `.gitattributes` stores the
  // file LF and checks it out CRLF (issue #2044), and an edit sends the bytes on
  // disk. The rule must not depend on which ending it gets.
  put(
    'scripts/check-merge.ps1',
    'Write-Output "probe"\r\nif (git diff --quiet) { Write-Output "unchanged" }\r\n',
  );
  const crlf = checkDiff(cwd);
  assert.equal(crlf.length, 1, crlf.join('\n'));
  assert.match(crlf[0], /scripts\/check-merge\.ps1.*powershell-if-native-command/);
});

test('checkAgentRules returns content-rule and line-ending failures together', t => {
  const { cwd, git, put } = repo(t);
  // Each suite alone sees only its own defect: the untracked .tsx is a content
  // violation, and the staged CRLF file is an index violation. Together they are
  // what the CLI — and therefore `npm run check:agent`, the harness fast gate
  // and CI — has to report.
  put('src/components/NewTerminal.ts', 'terminal.dispose();\n');
  mkdirSync(join(cwd, 'src/lib'), { recursive: true });
  put('src/lib/staged.ts', 'export const a = 1;\r\nexport const b = 2;\r\n');
  git('add', 'src/lib/staged.ts');

  const content = checkDiff(cwd);
  assert.equal(content.length, 1, content.join('\n'));
  assert.match(content[0], /NewTerminal\.ts.*terminal-dispose/);

  const both = checkAgentRules(cwd);
  assert.equal(both.skipped, false);
  assert.equal(both.failures.length, 2, both.failures.join('\n'));
  assert.ok(both.failures.some((failure) => /NewTerminal\.ts.*terminal-dispose/.test(failure)));
  assert.ok(both.failures.some((failure) => /src\/lib\/staged\.ts.*i\/crlf/.test(failure)));

  const run = spawnSync(process.execPath, [checker], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 1);
  assert.match(run.stderr, /terminal-dispose/);
  assert.match(run.stderr, /staged\.ts: stored in the index with CRLF line endings/);
});

test('configured hooks exist and shared skill entrypoints are readable', () => {
  const settings = JSON.parse(readFileSync(join(root, '.claude/settings.json'), 'utf8'));
  for (const entries of Object.values(settings.hooks)) {
    for (const entry of entries) for (const hook of entry.hooks) {
      const relative = hook.command.match(/\$CLAUDE_PROJECT_DIR\/([^" ]+)/)?.[1];
      assert.ok(relative, `Unrecognized hook path: ${hook.command}`);
      assert.ok(readFileSync(join(root, relative), 'utf8').length);
    }
  }
  for (const name of ['verify', 'verify-ui', 'use']) {
    const skill = readFileSync(join(root, `.claude/skills/${name}/SKILL.md`), 'utf8');
    assert.match(skill, new RegExp(`^---\\r?\\nname: ${name}\\r?\\n`));
    assert.match(skill, /\ndescription: .+/);
  }
});

test('Rust layout checks use the full item and added-line locations across committed and local edits', t => {
  const { cwd, git, put } = repo(t);
  mkdirSync(join(cwd, 'src-tauri/src'), { recursive: true });
  put('src-tauri/src/owner.rs', '#[cfg(test)]\nmod tests {\n    #[test] fn legacy() {}\n}\nfn production() {}\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'legacy tests');
  const base = git('rev-parse', 'HEAD').trim();
  put('src-tauri/src/owner.rs', '#[cfg(test)]\nmod tests {\n    #[test] fn legacy() {}\n}\nfn production() { let changed = true; }\n');
  assert.deepEqual(checkDiff(cwd, base), [], 'unchanged legacy tests stay exempt');
  put('src-tauri/src/owner.rs', '#[cfg(test)]\nmod tests {\n    #[test] fn legacy() {}\n    fn new_fixture() {}\n}\nfn production() {}\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'new fixture inside a legacy module');
  assert.match(checkDiff(cwd, base).join('\n'), /owner\.rs:4:.*rust-test-layout/);
  put('src-tauri/src/new.rs', '#[cfg(test)]\nstruct FakeHost;\n');
  const failures = checkDiff(cwd, base);
  assert.equal(failures.length, 2, failures.join('\n'));
  assert.match(failures.join('\n'), /new\.rs:1:.*rust-test-layout/);
  const run = spawnSync(process.execPath, [checker, '--base', base], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 1, run.stdout);
  assert.match(run.stderr, /rust-test-layout/);
});
