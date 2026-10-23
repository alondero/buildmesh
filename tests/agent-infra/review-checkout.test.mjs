import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { withReviewCheckout } from '../../scripts/review-checkout.mjs';

const cli = fileURLToPath(new URL('../../scripts/review-checkout.mjs', import.meta.url));

function repo(t) {
  const root = mkdtempSync(join(tmpdir(), 'buildmesh-review-'));
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  const put = (file, data) => { mkdirSync(join(root, file, '..'), { recursive: true }); writeFileSync(join(root, file), data); };
  git('init', '-q');
  git('config', 'user.name', 'Review test');
  git('config', 'user.email', 'review@example.invalid');
  git('config', 'core.autocrlf', 'false');
  git('config', 'core.hooksPath', join(root, 'no-hooks'));
  put('.gitignore', 'node_modules/\n.tmp/\ndist/\n');
  put('source.txt', 'committed revision\n');
  put('package.json', '{"type":"module"}');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture');
  const head = git('rev-parse', 'HEAD');
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { root, head, git, put };
}

test('a detached review preserves Git metadata and the requested commit without copying local edits', async t => {
  const fixture = repo(t);
  fixture.put('source.txt', 'local edit\n');
  const before = fixture.git('status', '--porcelain');
  let owned;
  await withReviewCheckout(fixture.root, fixture.head, async ({ path, revision }) => {
    owned = path;
    assert.equal(revision, fixture.head);
    assert.ok(existsSync(join(path, '.git')));
    assert.equal(execFileSync('git', ['rev-parse', 'HEAD'], { cwd: path, encoding: 'utf8' }).trim(), fixture.head);
    assert.equal(readFileSync(join(path, 'source.txt'), 'utf8'), 'committed revision\n');
    assert.equal(execFileSync('git', ['branch', '--show-current'], { cwd: path, encoding: 'utf8' }).trim(), '');
    writeFileSync(join(path, 'source.txt'), 'review-only edit\n');
  });
  assert.equal(existsSync(owned), false);
  assert.equal(fixture.git('status', '--porcelain'), before);
  assert.equal(readFileSync(join(fixture.root, 'source.txt'), 'utf8'), 'local edit\n');
  assert.equal(fixture.git('worktree', 'list', '--porcelain').split('\n').filter(line => line.startsWith('worktree ')).length, 1);
});

test('dependency packages are readable while cache roots, bin shims and Cargo outputs stay local', async t => {
  const fixture = repo(t);
  fixture.put('node_modules/example/package.json', '{"name":"example","version":"1.0.0"}');
  fixture.put('node_modules/@scope/example/package.json', '{"name":"@scope/example"}');
  fixture.put('node_modules/.bin/example.cmd', 'original shim');
  fixture.put('node_modules/.vite/existing', 'source cache');
  fixture.put('node_modules/.vite-temp/existing', 'source temporary cache');
  await withReviewCheckout(fixture.root, fixture.head, async ({ path, env }) => {
    assert.match(readFileSync(join(path, 'node_modules/example/package.json'), 'utf8'), /1\.0\.0/);
    assert.match(readFileSync(join(path, 'node_modules/@scope/example/package.json'), 'utf8'), /@scope/);
    assert.equal(existsSync(join(path, 'node_modules/.vite/existing')), false);
    assert.equal(existsSync(join(path, 'node_modules/.vite-temp/existing')), false);
    mkdirSync(join(path, 'node_modules/.vite'), { recursive: true });
    writeFileSync(join(path, 'node_modules/.vite/new'), 'review cache');
    writeFileSync(join(path, 'node_modules/.bin/example.cmd'), 'review shim');
    assert.ok(env.CARGO_TARGET_DIR.startsWith(path));
    assert.ok(env.npm_config_cache.startsWith(path));
    assert.ok(env.TMP.startsWith(path));
    assert.equal(env.TEMP, env.TMP);
  });
  assert.equal(readFileSync(join(fixture.root, 'node_modules/.bin/example.cmd'), 'utf8'), 'original shim');
  assert.equal(readFileSync(join(fixture.root, 'node_modules/.vite/existing'), 'utf8'), 'source cache');
  assert.equal(existsSync(join(fixture.root, 'node_modules/.vite/new')), false);
});

test('failed checks and unavailable refs clean up without deleting an unrelated worktree', async t => {
  const fixture = repo(t);
  let owned;
  await assert.rejects(withReviewCheckout(fixture.root, fixture.head, async ({ path }) => {
    owned = path;
    throw new Error('check failed');
  }), /check failed/);
  assert.equal(existsSync(owned), false);
  await assert.rejects(withReviewCheckout(fixture.root, 'missing-ref', async () => assert.fail('must not run')), /missing-ref/);
  assert.equal(fixture.git('worktree', 'list', '--porcelain').split('\n').filter(line => line.startsWith('worktree ')).length, 1);
});

test('cleanup survives a dependency link that dangles or was replaced by a real directory', async t => {
  const fixture = repo(t);
  fixture.put('node_modules/gone/package.json', '{"name":"gone"}');
  fixture.put('node_modules/swapped/package.json', '{"name":"swapped"}');
  let owned;
  await withReviewCheckout(fixture.root, fixture.head, async ({ path }) => {
    owned = path;
    rmSync(join(fixture.root, 'node_modules/gone'), { recursive: true, force: true }); // the link now dangles
    unlinkSync(join(path, 'node_modules/swapped'));
    mkdirSync(join(path, 'node_modules/swapped'));
    writeFileSync(join(path, 'node_modules/swapped/own.txt'), 'installed in the review checkout');
  });
  assert.equal(existsSync(owned), false);
  assert.ok(existsSync(join(fixture.root, 'node_modules/swapped/package.json')));
  assert.equal(fixture.git('worktree', 'list', '--porcelain').split('\n').filter(line => line.startsWith('worktree ')).length, 1);
});

test('the CLI propagates command failure, preserves argument bytes and prints the reviewed revision', t => {
  const fixture = repo(t);
  const run = spawnSync(process.execPath, [cli, '--ref', fixture.head, '--', 'node', '-e', 'console.log(process.argv[1]); process.exit(7)', 'spaces $HOME `literal`'], { cwd: fixture.root, encoding: 'utf8' });
  assert.equal(run.status, 7, run.stderr);
  assert.match(run.stdout, new RegExp(fixture.head));
  assert.ok(run.stdout.includes('spaces $HOME `literal`'));
  assert.equal(fixture.git('worktree', 'list', '--porcelain').split('\n').filter(line => line.startsWith('worktree ')).length, 1);
});

test('dependency changes require a private install and cleanup still runs after setup fails', async t => {
  const fixture = repo(t);
  fixture.put('node_modules/example/package.json', '{"name":"example"}');
  fixture.put('package.json', '{"type":"module","dependencies":{"example":"2"}}');
  await assert.rejects(withReviewCheckout(fixture.root, fixture.head, async () => assert.fail('must not run')), /Dependency inputs differ.*--no-deps/);
  await withReviewCheckout(fixture.root, fixture.head, async ({ path }) => {
    assert.equal(existsSync(join(path, 'node_modules/example')), false);
  }, { dependencies: false });
  const install = spawnSync(process.execPath, [cli, '--', 'npm', 'ci'], { cwd: fixture.root, encoding: 'utf8' });
  assert.equal(install.status, 1);
  assert.match(install.stderr, /installation needs --no-deps/);
  assert.equal(fixture.git('worktree', 'list', '--porcelain').split('\n').filter(line => line.startsWith('worktree ')).length, 1);
});
