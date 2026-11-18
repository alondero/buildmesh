import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { exactTestBatches, listedUnitTests, partitionUnitTests, readShards, repoRoot } from '../../scripts/ci/rust-shards.mjs';

test('the shard list is read from the real verify workflow', () => {
  const shards = readShards();
  assert.deepEqual(shards.map((shard) => shard.label), [
    'db',
    'services',
    'agent',
    'commands-http',
    'circuit-coordinator',
    'git-env-preferences',
    'remaining',
  ]);
  assert.deepEqual(shards.find((shard) => shard.label === 'db').modules, ['db']);
});

test('a matrix line the parser does not understand fails loudly instead of dropping a shard', () => {
  const workflow = [
    '  rust-tests:',
    '    strategy:',
    '      matrix:',
    '        shard:',
    '          - label: db',
    '            modules: "db"',
    '          - label: broken',
    '            filters: "services::"',
    '    steps:',
  ].join('\n');
  assert.throws(() => readShards(workflow), /does not understand/);
  assert.throws(() => readShards('jobs:\n  quality:\n'), /no `rust-tests` job/);
});

test('new top-level modules and nested matches go to exactly one owner', () => {
  const names = listedUnitTests([
    'commands::diagnostics::starts: test',
    'artifact_owner::identifies_test_binary: test',
    'test review_contract::keeps_identity: test',
    'worktree_blockers::shows_reason: test',
    'blocking::worker: test',
    'read_outcome::typed_result: test',
  ].join('\n'));
  const plan = partitionUnitTests(readShards(), names);
  assert.deepEqual(plan.get('commands-http'), names.slice(0, 1));
  assert.deepEqual(plan.get('remaining'), names.slice(1));
  assert.ok(plan.get('remaining').includes('review_contract::keeps_identity'));
  assert.deepEqual(exactTestBatches(['short', 'lengthy'], 12), [['short'], ['lengthy']]);
});

test('duplicate module owners and a missing remainder are rejected', () => {
  const names = ['db::test'];
  assert.throws(() => partitionUnitTests([{ label: 'db', modules: ['db'] }], names), /wildcard owner/);
  assert.throws(() => partitionUnitTests([
    { label: 'db', modules: ['db'] },
    { label: 'other', modules: ['db'] },
  ], names), /Multiple shards own the Rust module db/);
  assert.throws(() => partitionUnitTests(readShards(), [...names, ...names]), /duplicate Rust test name/);
});

test('CI shard runner selects exact names from the compiled Cargo fixture', t => {
  const cwd = mkdtempSync(join(tmpdir(), 'buildmesh-rust-shard-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  writeFileSync(join(cwd, 'Cargo.toml'), '[package]\nname="shard-fixture"\nversion="0.1.0"\nedition="2021"\n');
  writeFileSync(join(cwd, 'Cargo.lock'), 'version = 4\n[[package]]\nname = "shard-fixture"\nversion = "0.1.0"\n');
  mkdirSync(join(cwd, 'src'));
  writeFileSync(join(cwd, 'src/lib.rs'), [
    'mod commands { mod diagnostics { #[test] fn nested() {} #[test] fn library_root() {} } }',
    'mod http { mod routes { mod git { #[test] fn nested() {} } } }',
    'mod moved_to_new_root { #[test] fn unclaimed() {} }',
    '#[test] fn library_root() {}',
  ].join('\n'));
  const script = join(repoRoot, 'scripts/ci/run-rust-shard.mjs');
  const env = { ...process.env, CARGO_TARGET_DIR: join(cwd, 'target') };
  const runShard = label => {
    const result = spawnSync(process.execPath, [script, label], { cwd, env, encoding: 'utf8' });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    return result.stdout;
  };

  const commands = runShard('commands-http');
  assert.match(commands, /test commands::diagnostics::nested \.\.\. ok/);
  assert.match(commands, /test commands::diagnostics::library_root \.\.\. ok/);
  assert.match(commands, /test http::routes::git::nested \.\.\. ok/);
  assert.match(commands, /3 passed/);

  const fallback = runShard('remaining');
  assert.match(fallback, /test moved_to_new_root::unclaimed \.\.\. ok/);
  assert.match(fallback, /test library_root \.\.\. ok/);
  assert.match(fallback, /2 passed/);

  assert.match(runShard('git-env-preferences'), /0 assigned tests/);
});
