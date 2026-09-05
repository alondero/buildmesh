import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkDiff } from '../../scripts/check-agent-diff.mjs';

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

test('checks committed changes against the requested base and fails on a missing base', t => {
  const { cwd, git, put } = repo(t);
  const base = git('rev-parse', 'HEAD').trim();
  put('src/components/Terminal.ts', 'terminal.dispose();\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'change');
  assert.deepEqual(checkDiff(cwd), []);
  assert.equal(checkDiff(cwd, base).length, 1);
  const run = spawnSync(process.execPath, [checker, '--base', 'missing-base'], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 1);
  assert.match(run.stderr, /Agent diff check failed/);
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
