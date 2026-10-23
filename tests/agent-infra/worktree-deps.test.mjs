import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { linkWorktreeDependencies, mainCheckout, unlinkWorktreeDependencies } from '../../scripts/worktree-deps.mjs';

const harness = fileURLToPath(new URL('../../scripts/harness.mjs', import.meta.url));
const PACKAGE = '{"type":"module","dependencies":{"example":"1"}}';

// A main checkout with installed packages plus one secondary worktree.
function fixture(t) {
  const parent = realpathSync(mkdtempSync(join(tmpdir(), 'buildmesh-deps-')));
  const main = join(parent, 'main');
  const worktree = join(parent, 'wt');
  mkdirSync(main);
  const git = (cwd, ...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  const put = (file, data) => { mkdirSync(dirname(file), { recursive: true }); writeFileSync(file, data); };
  git(main, 'init', '-q');
  git(main, 'config', 'user.name', 'Deps test');
  git(main, 'config', 'user.email', 'deps@example.invalid');
  git(main, 'config', 'core.autocrlf', 'false');
  git(main, 'config', 'core.hooksPath', join(main, 'no-hooks'));
  put(join(main, '.gitignore'), 'node_modules/\nsrc-tauri/target/\n');
  put(join(main, 'package.json'), PACKAGE);
  put(join(main, 'package-lock.json'), '{"lockfileVersion":3}');
  git(main, 'add', '.');
  git(main, '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture');
  git(main, 'worktree', 'add', '-q', '--detach', worktree);
  put(join(main, 'node_modules/example/package.json'), '{"name":"example","version":"1.0.0"}');
  put(join(main, 'node_modules/@scope/example/package.json'), '{"name":"@scope/example"}');
  put(join(main, 'node_modules/.bin/example.cmd'), 'original shim');
  put(join(main, 'node_modules/.vite/existing'), 'main cache');
  put(join(main, 'src-tauri/target/debug/artifact.bin'), 'compiled');
  t.after(() => {
    // Detach links first so cleanup can never reach the shared fixture packages.
    for (const link of [join(worktree, 'node_modules/example'), join(worktree, 'node_modules/@scope/example'), join(worktree, 'node_modules'), join(worktree, 'src-tauri/target')]) {
      try { if (lstatSync(link).isSymbolicLink()) unlinkSync(link); } catch { /* absent */ }
    }
    rmSync(parent, { recursive: true, force: true });
  });
  return { parent, main, worktree, git, put, mainPackage: join(main, 'node_modules/example/package.json') };
}

const isLink = path => { try { return lstatSync(path).isSymbolicLink(); } catch { return false; } };

test('a secondary worktree finds the main checkout from git worktree list', t => {
  const f = fixture(t);
  assert.equal(realpathSync(mainCheckout(f.worktree)), f.main);
  assert.throws(() => linkWorktreeDependencies(f.main), /main checkout/i);
});

test('linking exposes packages without npm ci while .bin and caches stay local', t => {
  const f = fixture(t);
  const summary = linkWorktreeDependencies(f.worktree);
  assert.equal(realpathSync(summary.source), f.main);
  assert.match(readFileSync(join(f.worktree, 'node_modules/example/package.json'), 'utf8'), /1\.0\.0/);
  assert.match(readFileSync(join(f.worktree, 'node_modules/@scope/example/package.json'), 'utf8'), /@scope/);
  assert.ok(isLink(join(f.worktree, 'node_modules/example')));
  assert.equal(isLink(join(f.worktree, 'node_modules')), false, 'the node_modules root must stay private');
  assert.equal(existsSync(join(f.worktree, 'node_modules/.vite/existing')), false);
  writeFileSync(join(f.worktree, 'node_modules/.bin/example.cmd'), 'worktree shim');
  assert.equal(readFileSync(join(f.main, 'node_modules/.bin/example.cmd'), 'utf8'), 'original shim');
});

test('linking again refreshes our own links instead of failing', t => {
  const f = fixture(t);
  linkWorktreeDependencies(f.worktree);
  f.put(join(f.main, 'node_modules/added/package.json'), '{"name":"added"}');
  linkWorktreeDependencies(f.worktree);
  assert.ok(existsSync(join(f.worktree, 'node_modules/added/package.json')));
});

test('unlinking removes every link, never touches the main packages and is repeatable', t => {
  const f = fixture(t);
  linkWorktreeDependencies(f.worktree);
  const before = readFileSync(f.mainPackage, 'utf8');
  const summary = unlinkWorktreeDependencies(f.worktree);
  assert.ok(summary.removed >= 2);
  assert.equal(existsSync(join(f.worktree, 'node_modules/example')), false);
  assert.equal(existsSync(join(f.worktree, 'node_modules/@scope')), false);
  assert.equal(readFileSync(f.mainPackage, 'utf8'), before);
  assert.ok(existsSync(join(f.main, 'node_modules/@scope/example/package.json')));
  assert.equal(readFileSync(join(f.main, 'node_modules/.bin/example.cmd'), 'utf8'), 'original shim');
  assert.equal(unlinkWorktreeDependencies(f.worktree).removed, 0);
});

test('a manual whole-directory junction is unlinked without deleting the shared packages', t => {
  const f = fixture(t);
  symlinkSync(join(f.main, 'node_modules'), join(f.worktree, 'node_modules'), process.platform === 'win32' ? 'junction' : 'dir');
  const summary = unlinkWorktreeDependencies(f.worktree);
  assert.equal(summary.removed, 1);
  assert.equal(existsSync(join(f.worktree, 'node_modules')), false);
  assert.ok(existsSync(f.mainPackage));
  assert.ok(existsSync(join(f.main, 'node_modules/@scope/example/package.json')));
});

test('a real npm install is neither relinked over nor unlinked', t => {
  const f = fixture(t);
  f.put(join(f.worktree, 'node_modules/real/package.json'), '{"name":"real"}');
  assert.throws(() => linkWorktreeDependencies(f.worktree), /npm ci|already exists/);
  assert.equal(unlinkWorktreeDependencies(f.worktree).removed, 0);
  assert.ok(existsSync(join(f.worktree, 'node_modules/real/package.json')));
});

test('differing dependency declarations refuse to link and point at npm ci', t => {
  const f = fixture(t);
  f.put(join(f.worktree, 'package.json'), '{"type":"module","dependencies":{"example":"2"}}');
  assert.throws(() => linkWorktreeDependencies(f.worktree), /package\.json.*npm ci/s);
  assert.equal(existsSync(join(f.worktree, 'node_modules')), false);
});

test('a changed lockfile refuses to link', t => {
  const f = fixture(t);
  f.put(join(f.worktree, 'package-lock.json'), '{"lockfileVersion":3,"packages":{"x":{}}}');
  assert.throws(() => linkWorktreeDependencies(f.worktree), /package-lock\.json.*npm ci/s);
  assert.equal(existsSync(join(f.worktree, 'node_modules')), false);
});

test('a source without installed packages is an actionable error', t => {
  const f = fixture(t);
  rmSync(join(f.main, 'node_modules'), { recursive: true, force: true });
  assert.throws(() => linkWorktreeDependencies(f.worktree), /npm ci/);
});

test('--from selects the dependency source explicitly', t => {
  const f = fixture(t);
  const other = join(f.parent, 'other');
  mkdirSync(other);
  for (const file of ['package.json', 'package-lock.json']) writeFileSync(join(other, file), readFileSync(join(f.main, file)));
  f.put(join(other, 'node_modules/example/package.json'), '{"name":"example","version":"9.9.9"}');
  linkWorktreeDependencies(f.worktree, { from: other });
  assert.match(readFileSync(join(f.worktree, 'node_modules/example/package.json'), 'utf8'), /9\.9\.9/);
  assert.throws(() => linkWorktreeDependencies(f.worktree, { from: join(f.parent, 'missing') }), /missing/);
});

test('the Cargo target is only linked on request and unlinks without touching main artifacts', t => {
  const f = fixture(t);
  linkWorktreeDependencies(f.worktree);
  assert.equal(existsSync(join(f.worktree, 'src-tauri/target')), false);
  unlinkWorktreeDependencies(f.worktree);
  const summary = linkWorktreeDependencies(f.worktree, { cargoTarget: true });
  assert.equal(summary.cargoTarget, true);
  assert.ok(isLink(join(f.worktree, 'src-tauri/target')));
  assert.ok(existsSync(join(f.worktree, 'src-tauri/target/debug/artifact.bin')));
  unlinkWorktreeDependencies(f.worktree);
  assert.equal(existsSync(join(f.worktree, 'src-tauri/target')), false);
  assert.equal(readFileSync(join(f.main, 'src-tauri/target/debug/artifact.bin'), 'utf8'), 'compiled');
});

test('a populated worktree Cargo target is never replaced by a link', t => {
  const f = fixture(t);
  f.put(join(f.worktree, 'src-tauri/target/own.bin'), 'mine');
  assert.throws(() => linkWorktreeDependencies(f.worktree, { cargoTarget: true }), /src-tauri[\\/]target/);
  assert.equal(readFileSync(join(f.worktree, 'src-tauri/target/own.bin'), 'utf8'), 'mine');
});

test('a tampered marker cannot make unlink-deps remove links outside node_modules', t => {
  const f = fixture(t);
  linkWorktreeDependencies(f.worktree);
  const outside = join(f.parent, 'outside');
  mkdirSync(join(f.parent, 'victim-target'));
  symlinkSync(join(f.parent, 'victim-target'), outside, process.platform === 'win32' ? 'junction' : 'dir');
  mkdirSync(join(f.parent, 'emptydir'));
  writeFileSync(join(f.worktree, 'node_modules/.buildmesh-linked-deps.json'), JSON.stringify({ links: ['../../outside', '../../emptydir/zz', 'example'] }));
  unlinkWorktreeDependencies(f.worktree);
  assert.ok(isLink(outside), 'a link outside node_modules must survive');
  assert.ok(existsSync(join(f.parent, 'emptydir')), 'a directory outside node_modules must survive');
  assert.equal(isLink(join(f.worktree, 'node_modules/example')), false);
  unlinkSync(outside);
});

test('unlink-deps cleans up after a link-deps that was interrupted before finishing', t => {
  const f = fixture(t);
  const modules = join(f.worktree, 'node_modules');
  mkdirSync(join(modules, '@scope'), { recursive: true });
  writeFileSync(join(modules, '.buildmesh-linked-deps.json'), JSON.stringify({ source: f.main }));
  symlinkSync(join(f.main, 'node_modules/example'), join(modules, 'example'), process.platform === 'win32' ? 'junction' : 'dir');
  symlinkSync(join(f.main, 'node_modules/@scope/example'), join(modules, '@scope/example'), process.platform === 'win32' ? 'junction' : 'dir');
  assert.equal(unlinkWorktreeDependencies(f.worktree).removed, 2);
  assert.equal(existsSync(modules), false);
  assert.ok(existsSync(f.mainPackage));
  linkWorktreeDependencies(f.worktree);
  assert.ok(isLink(join(modules, 'example')));
});

test('a link failure part-way rolls back every link and scope directory it made', t => {
  const f = fixture(t);
  f.put(join(f.main, 'node_modules/@zzbad'), 'a file where a scope directory should be');
  assert.throws(() => linkWorktreeDependencies(f.worktree));
  assert.equal(existsSync(join(f.worktree, 'node_modules')), false);
  assert.ok(existsSync(f.mainPackage));
  rmSync(join(f.main, 'node_modules/@zzbad'));
  linkWorktreeDependencies(f.worktree);
  assert.ok(isLink(join(f.worktree, 'node_modules/example')));
});

test('a Cargo target conflict leaves an existing working link set untouched', t => {
  const f = fixture(t);
  linkWorktreeDependencies(f.worktree);
  f.put(join(f.worktree, 'src-tauri/target/own.bin'), 'mine');
  assert.throws(() => linkWorktreeDependencies(f.worktree, { cargoTarget: true }), /src-tauri[\\/]target/);
  assert.ok(existsSync(join(f.worktree, 'node_modules/example/package.json')));
});

test('harness link-deps and unlink-deps run non-interactively with a clear exit status', t => {
  const f = fixture(t);
  const run = (...args) => spawnSync(process.execPath, [harness, ...args], { cwd: f.worktree, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout: 60_000 });
  const linked = run('link-deps');
  assert.equal(linked.status, 0, linked.stderr);
  assert.ok(existsSync(join(f.worktree, 'node_modules/example/package.json')));
  const unlinked = run('unlink-deps');
  assert.equal(unlinked.status, 0, unlinked.stderr);
  assert.equal(existsSync(join(f.worktree, 'node_modules/example')), false);
  assert.ok(existsSync(f.mainPackage));
  f.put(join(f.worktree, 'package.json'), '{"type":"module"}');
  const refused = run('link-deps');
  assert.equal(refused.status, 2);
  assert.match(refused.stderr, /npm ci/);
  assert.equal(run('link-deps', '--bogus').status, 2);
});
