import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkLineEndings, parseEolRecords } from '../../scripts/check-line-endings.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const checker = join(root, 'scripts/check-line-endings.mjs');

// No `.gitattributes` and core.autocrlf=false, so `git add` stores the bytes it
// is handed. That is the only way to build the pre-issue-#2044 index states the
// gate exists to catch, and it keeps every case below independent of the
// repository's own policy.
function repo(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'buildmesh-eol-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  git('init', '-q');
  git('config', 'user.name', 'Agent test');
  git('config', 'user.email', 'agent-test@example.invalid');
  git('config', 'core.autocrlf', 'false');
  git('config', 'core.hooksPath', join(cwd, 'no-hooks'));
  return { cwd, git };
}

function stage(cwd, git, file, bytes) {
  mkdirSync(dirname(join(cwd, file)), { recursive: true });
  writeFileSync(join(cwd, file), bytes);
  git('add', file);
}

test('flags text stored with CRLF or mixed line endings, and leaves LF alone', t => {
  const { cwd, git } = repo(t);
  stage(cwd, git, 'clean.ts', 'const a = 1;\nconst b = 2;\n');
  assert.deepEqual(checkLineEndings(cwd), []);

  stage(cwd, git, 'crlf.ts', 'const a = 1;\r\nconst b = 2;\r\n');
  stage(cwd, git, 'mixed.ts', 'const a = 1;\r\nconst b = 2;\n');
  const failures = checkLineEndings(cwd);
  assert.equal(failures.length, 2, failures.join('\n'));
  assert.match(failures.join('\n'), /crlf\.ts: stored in the index with CRLF line endings \(i\/crlf\)/);
  assert.match(failures.join('\n'), /mixed\.ts: stored in the index with mixed line endings \(i\/mixed\)/);
  assert.ok(failures.every((failure) => failure.includes('git add --renormalize')));
});

test('exempts binary files, empty files, symlinks and a file marked -text', t => {
  const { cwd, git } = repo(t);
  writeFileSync(join(cwd, '.gitattributes'), 'seeds/** -text\n');
  mkdirSync(join(cwd, 'seeds'), { recursive: true });
  // A binary is never converted, so git still reports the endings it holds; the
  // fuzz corpus seeds are the real instance of this and must stay green. They
  // carry CRLF and an invalid UTF-8 byte but no NUL, so git's own heuristic
  // still calls them text and reports `i/crlf` -- which is why the exemption has
  // to come from the `-text` attribute rather than from the kind.
  stage(cwd, git, 'seeds/http-request.bin', Buffer.from([0x48, 0x0d, 0x0a, 0x52, 0x0d, 0x0a, 0xff]));
  // Detected as binary by content (the NUL byte), which is the other exemption
  // route: no `-text` attribute, git's own heuristic reports `i/-text`.
  stage(cwd, git, 'logo.png', Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00]));
  stage(cwd, git, 'empty.txt', '');
  const link = execFileSync('git', ['hash-object', '-w', '--stdin'], { cwd, input: 'CLAUDE.md', encoding: 'utf8' }).trim();
  git('update-index', '--add', '--cacheinfo', `120000,${link},AGENTS.md`);
  assert.deepEqual(checkLineEndings(cwd), []);

  const kinds = parseEolRecords(
    execFileSync('git', ['ls-files', '--eol', '-z'], { cwd, encoding: 'utf8' }),
  );
  assert.equal(kinds.find((record) => record.path === 'AGENTS.md').index, '');
  assert.equal(kinds.find((record) => record.path === 'empty.txt').index, 'none');
  assert.equal(kinds.find((record) => record.path === 'logo.png').index, '-text');
  assert.equal(kinds.find((record) => record.path === 'seeds/http-request.bin').index, 'crlf');
  assert.equal(kinds.find((record) => record.path === 'seeds/http-request.bin').binary, true);

  // The exemption is attribute-driven, not kind-driven: remove the attribute
  // and the same bytes become a violation.
  writeFileSync(join(cwd, '.gitattributes'), '# no binary rule\n');
  git('add', '.gitattributes');
  const failures = checkLineEndings(cwd);
  assert.equal(failures.length, 1, failures.join('\n'));
  assert.match(failures[0], /seeds\/http-request\.bin/);
});

test('the Windows script exception stays LF in the index and CRLF in the worktree', t => {
  const { cwd, git } = repo(t);
  writeFileSync(join(cwd, '.gitattributes'), '* text=auto eol=lf\n*.ps1 text eol=crlf\n');
  stage(cwd, git, 'run.ps1', 'Write-Output "ok"\n');
  stage(cwd, git, 'app.ts', 'export const a = 1;\n');
  assert.deepEqual(checkLineEndings(cwd), []);

  // `git add` never rewrites the worktree, so the smudge rule is only observable
  // after a real checkout. This is the property the policy exists for: the
  // stored form stays LF while cmd.exe and PowerShell get CRLF.
  rmSync(join(cwd, 'run.ps1'));
  rmSync(join(cwd, 'app.ts'));
  git('checkout', '--', 'run.ps1', 'app.ts');
  const records = parseEolRecords(
    execFileSync('git', ['ls-files', '--eol', '-z'], { cwd, encoding: 'utf8' }),
  );
  const script = records.find((record) => record.path === 'run.ps1');
  const source = records.find((record) => record.path === 'app.ts');
  assert.equal(script.index, 'lf');
  assert.equal(script.worktree, 'crlf');
  assert.equal(source.index, 'lf');
  assert.equal(source.worktree, 'lf');
});

test('reports a path containing spaces in full and exits non-zero', t => {
  const { cwd, git } = repo(t);
  stage(cwd, git, 'src/has space.ts', 'export const a = 1;\r\n');
  const failures = checkLineEndings(cwd);
  assert.equal(failures.length, 1, failures.join('\n'));
  assert.ok(failures[0].startsWith('src/has space.ts: '), failures[0]);

  const run = spawnSync(process.execPath, [checker], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 1);
  assert.match(run.stderr, /Line-ending check failed \(1 issue\)/);
  assert.match(run.stderr, /src\/has space\.ts/);
});

test('the CLI passes on a clean tree and rejects a stray argument', t => {
  const { cwd, git } = repo(t);
  stage(cwd, git, 'app.ts', 'export const a = 1;\n');
  const pass = spawnSync(process.execPath, [checker], { cwd, encoding: 'utf8' });
  assert.equal(pass.status, 0, pass.stderr);
  assert.match(pass.stdout, /stored LF in the index/);

  const misused = spawnSync(process.execPath, [checker, '--base', 'HEAD'], { cwd, encoding: 'utf8' });
  assert.equal(misused.status, 1);
  assert.match(misused.stderr, /Usage: node scripts\/check-line-endings\.mjs/);
});

test('a checkout that is not a git repository fails instead of reporting success', t => {
  const cwd = mkdtempSync(join(tmpdir(), 'buildmesh-eol-bare-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const run = spawnSync(process.execPath, [checker], { cwd, encoding: 'utf8' });
  assert.equal(run.status, 1);
  assert.match(run.stderr, /Line-ending check failed/);
});

test('this repository declares the policy and its index obeys it', () => {
  const declared = readFileSync(join(root, '.gitattributes'), 'utf8').split('\n');
  for (const line of ['* text=auto eol=lf', '*.ps1 text eol=crlf', '*.cmd text eol=crlf', '*.bat text eol=crlf']) {
    assert.ok(declared.includes(line), `.gitattributes no longer declares "${line}"`);
  }
  assert.deepEqual(checkLineEndings(root), []);
});