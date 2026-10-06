import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { formatTouched } from '../../scripts/rustfmt-touched.mjs';
import { decide } from '../../.claude/hooks/guard-rustfmt.mjs';

const tool = fileURLToPath(new URL('../../scripts/rustfmt-touched.mjs', import.meta.url));
const hookPath = fileURLToPath(new URL('../../.claude/hooks/guard-rustfmt.mjs', import.meta.url));

function crate(t) {
  const cwd = mkdtempSync(join(tmpdir(), `buildmesh-rustfmt-${process.pid}-`));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  git('init', '-q');
  git('config', 'user.name', 'Rustfmt test');
  git('config', 'user.email', 'rustfmt@example.invalid');
  git('config', 'core.autocrlf', 'false');
  const put = (path, data) => { mkdirSync(join(cwd, path, '..'), { recursive: true }); writeFileSync(join(cwd, path), data); };
  put('src/lib.rs', 'mod child;\nfn  owner() {}\n');
  put('src/child.rs', 'fn  child() {}\r\nfn  mixed() {}\n');
  put('src/other.rs', 'fn  other() {}\n');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture');
  return { cwd, put, read: path => readFileSync(join(cwd, path)) };
}

// rustfmt on a module root also rewrites its child modules; the fake formatter does exactly that.
const cascading = (cwd, paths = ['src/lib.rs', 'src/child.rs', 'src/other.rs']) => () => {
  for (const path of paths.filter(path => existsSync(join(cwd, path)))) writeFileSync(join(cwd, path), `// formatted\n${readFileSync(join(cwd, path), 'latin1').replace(/fn {2}/g, 'fn ')}`, 'latin1');
};

test('formatTouched keeps the requested file formatted and restores every other Rust file byte for byte', t => {
  const fixture = crate(t);
  const before = { child: fixture.read('src/child.rs'), other: fixture.read('src/other.rs') };
  const result = formatTouched(fixture.cwd, ['src/lib.rs'], cascading(fixture.cwd));
  assert.match(fixture.read('src/lib.rs').toString(), /^\/\/ formatted\nmod child;\nfn owner\(\) \{\}/);
  assert.deepEqual(fixture.read('src/child.rs'), before.child);
  assert.deepEqual(fixture.read('src/other.rs'), before.other);
  assert.deepEqual(result, { formatted: ['src/lib.rs'], restored: ['src/child.rs', 'src/other.rs'] });
});

test('formatTouched restores a sibling that already had uncommitted edits to its edited content, not to git HEAD', t => {
  const fixture = crate(t);
  fixture.put('src/other.rs', 'fn  other() { /* work in progress */ }\n');
  const edited = fixture.read('src/other.rs');
  formatTouched(fixture.cwd, ['src/lib.rs'], cascading(fixture.cwd));
  assert.deepEqual(fixture.read('src/other.rs'), edited);
});

test('formatTouched leaves untouched files alone when the formatter changes only the requested file', t => {
  const fixture = crate(t);
  const result = formatTouched(fixture.cwd, ['src/other.rs'], () => fixture.put('src/other.rs', 'fn other() {}\n'));
  assert.deepEqual(result, { formatted: ['src/other.rs'], restored: [] });
});

test('formatTouched matches the requested file however the path is spelled', t => {
  for (const spell of [path => `./${path}`, path => path.replaceAll('/', '\\'), (path, cwd) => join(cwd, path)]) {
    const fixture = crate(t);
    const result = formatTouched(fixture.cwd, [spell('src/lib.rs', fixture.cwd)], cascading(fixture.cwd));
    assert.deepEqual(result, { formatted: ['src/lib.rs'], restored: ['src/child.rs', 'src/other.rs'] });
    assert.match(fixture.read('src/lib.rs').toString(), /^\/\/ formatted\n/, 'the requested file must stay formatted');
  }
});

test('formatTouched works in a dirty tree: a tracked file deleted on disk is skipped, an untracked sibling is restored', t => {
  const fixture = crate(t);
  unlinkSync(join(fixture.cwd, 'src/other.rs'));
  fixture.put('src/fresh.rs', 'fn  fresh() {}\n');
  const fresh = fixture.read('src/fresh.rs');
  const result = formatTouched(fixture.cwd, ['src/lib.rs'], cascading(fixture.cwd, ['src/lib.rs', 'src/child.rs', 'src/fresh.rs']));
  assert.deepEqual(fixture.read('src/fresh.rs'), fresh);
  assert.deepEqual(result.restored, ['src/child.rs', 'src/fresh.rs']);
  assert.equal(existsSync(join(fixture.cwd, 'src/other.rs')), false);
});

test('the rustfmt-touched command formats with the real rustfmt and leaves its child module untouched', t => {
  const fixture = crate(t);
  const child = fixture.read('src/child.rs');
  const run = spawnSync(process.execPath, [tool, 'src/lib.rs'], { cwd: fixture.cwd, encoding: 'utf8' });
  assert.equal(run.status, 0, run.stderr);
  assert.equal(fixture.read('src/lib.rs').toString(), 'mod child;\nfn owner() {}\n');
  assert.deepEqual(fixture.read('src/child.rs'), child);
  assert.match(run.stdout, /Formatted: src\/lib\.rs/);
});

test('formatTouched restores siblings even when the formatter fails after rewriting them', t => {
  const fixture = crate(t);
  const before = fixture.read('src/child.rs');
  assert.throws(() => formatTouched(fixture.cwd, ['src/lib.rs'], () => { cascading(fixture.cwd)(); throw new Error('rustfmt exited 1'); }), /rustfmt exited 1/);
  assert.deepEqual(fixture.read('src/child.rs'), before);
});

test('the rustfmt-touched command rejects non-Rust and missing paths before running a formatter', t => {
  const fixture = crate(t);
  for (const bad of ['package.json', 'src/missing.rs']) {
    const run = spawnSync(process.execPath, [tool, bad], { cwd: fixture.cwd, encoding: 'utf8' });
    assert.equal(run.status, 2, bad);
    assert.match(run.stderr, /\.rs/);
  }
  assert.equal(spawnSync(process.execPath, [tool], { cwd: fixture.cwd, encoding: 'utf8' }).status, 2);
});

test('the shell guard denies rustfmt and cargo fmt that would write, and names the safe command', () => {
  for (const command of [
    'rustfmt --edition 2021 src-tauri/src/lib.rs',
    'cd src-tauri && rustfmt src/lib.rs',
    'cargo fmt',
    'cargo fmt --all',
    'cargo fmt --manifest-path src-tauri/Cargo.toml',
    '& rustfmt src\\lib.rs',
    'C:\\Users\\dev\\.cargo\\bin\\rustfmt.exe src\\lib.rs',
    'cargo test && rustfmt --emit stdout src/lib.rs | tail -n +2 > src/lib.rs',
    'cargo +nightly fmt',
    'cargo --color never fmt',
    'cargo -q fmt --all',
    'FOO=1 rustfmt src/lib.rs',
    'env RUSTFLAGS=-Dwarnings rustfmt src/lib.rs',
    'time rustfmt src/lib.rs',
    'rtk rustfmt src/lib.rs',
    'rtk cargo fmt',
    'rustup run stable rustfmt src/lib.rs',
    'git ls-files "*.rs" | xargs rustfmt',
    'find src -name "*.rs" -exec rustfmt {} +',
    'find src -name "*.rs" -execdir rustfmt {} +',
    'bash -c "rustfmt src/lib.rs"',
    'cmd /c rustfmt src\\lib.rs',
    'powershell -Command "rustfmt src\\lib.rs"',
    '(rustfmt src/lib.rs)',
    'for f in a.rs b.rs; do rustfmt $f; done',
    'Get-ChildItem *.rs | ForEach-Object { rustfmt $_ }',
    '& "rustfmt" src\\lib.rs',
    'Rustfmt.exe src\\lib.rs',
    'rustfmt src/lib.rs # --check',
  ]) {
    const verdict = decide(command);
    assert.equal(verdict?.permissionDecision, 'deny', command);
    assert.match(verdict.permissionDecisionReason, /node scripts\/rustfmt-touched\.mjs/, command);
    assert.match(verdict.permissionDecisionReason, /--check/, command);
  }
});

test('the shell guard allows read-only format runs, the safe script, and prose that merely mentions rustfmt', () => {
  for (const command of [
    'rustfmt --edition 2021 --check src/lib.rs',
    'cargo fmt --all --check --manifest-path src-tauri/Cargo.toml',
    'cargo fmt -- --check',
    'rustfmt --version',
    'cargo fmt --help',
    'node scripts/rustfmt-touched.mjs src-tauri/src/lib.rs',
    'git commit -m "fix: stop rustfmt cascading into child modules"',
    'grep -n rustfmt scripts/harness.mjs',
    'cargo fmtcheck-helper',
    "gh pr create --body-file - <<'EOF'\nrustfmt src/lib.rs rewrites children\ncargo fmt rewrites the crate\nEOF",
    'cargo test --lib',
    'cargo build -p fmt',
    'echo "step one && rustfmt src/lib.rs"',
    'git commit -m "fix: stop the cascade\n\nrustfmt rewrites children; cargo fmt the whole crate"',
    'rustfmt --check src/lib.rs # then fix by hand',
  ]) assert.equal(decide(command), null, command);
});

test('the shell guard hook answers on stdin for Bash and PowerShell and ignores other tools', () => {
  const run = payload => spawnSync(process.execPath, [hookPath], { encoding: 'utf8', input: JSON.stringify(payload) });
  for (const tool_name of ['Bash', 'PowerShell']) {
    const denied = run({ tool_name, tool_input: { command: 'rustfmt src/lib.rs' } });
    assert.equal(denied.status, 0, denied.stderr);
    assert.equal(JSON.parse(denied.stdout).hookSpecificOutput.permissionDecision, 'deny');
    assert.equal(run({ tool_name, tool_input: { command: 'rustfmt --check src/lib.rs' } }).stdout, '');
  }
  assert.equal(run({ tool_name: 'Edit', tool_input: { command: 'rustfmt src/lib.rs' } }).stdout, '');
  assert.equal(spawnSync(process.execPath, [hookPath], { encoding: 'utf8', input: 'not json' }).status, 0);
});
